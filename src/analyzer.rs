use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet, VecDeque};
use std::fmt;
use std::path::{Path, PathBuf};

use indexmap::IndexMap;
use num_bigint::BigInt;
use num_rational::BigRational;
use num_traits::{One, ToPrimitive, Zero};
use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::ast::*;
use crate::diagnostic::{Diagnostic, Severity, SourceSpan, stable_fingerprint};
use crate::report::{
    AssumedZero, Completeness, Constraint, MetricBounds, MonitoredMetric, Obligation,
    ProvenanceRecord, ProvisionedResource, Report, ResourceSchedule, ResourceScheduleActivity,
    ResourceScheduleStage, ResourceUtilization, StreamCardinality, StreamHandoff, SymbolReport,
    TemporalMetric, TemporalRange,
};
use crate::source::{SourceError, SourceSet, line_column, load, load_with_overlays};
use crate::types::{
    Dimension, NumericRefinement, Type, TypeRegistry, Unit, compatible, parse_rational,
    signed_scalar_literal,
};

const CORE_CAPABILITIES: &[&str] = &[
    "Equatable",
    "Ordered",
    "Add",
    "Subtract",
    "Multiply",
    "Divide",
    "Power",
    "Iterable",
    "Indexable",
    "Copyable",
];

#[derive(Clone, Debug, Default)]
pub struct CompilerOptions {
    pub absolute_paths: bool,
    /// Root-process configuration overrides supplied by tooling.
    pub overrides: Vec<ConfigOverride>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ConfigOverride {
    pub path: String,
    pub value: String,
}

#[derive(Clone, Copy, Debug)]
pub enum InspectStage {
    Cst,
    Ast,
    Hir,
    Flow,
    Constraints,
    Temporal,
}

#[derive(Debug, Error)]
pub enum CompileError {
    #[error("{0}")]
    EntryModel(String),
    #[error(transparent)]
    Source(#[from] SourceError),
    #[error("entry module was not loaded")]
    MissingEntry,
    #[error("cannot serialize inspection output: {0}")]
    Serialize(#[from] serde_json::Error),
}

#[derive(Clone, Debug, Default, Serialize)]
pub struct SemanticModel {
    pub artifacts: IndexMap<String, ArtifactModel>,
    pub resources: IndexMap<String, ResourceModel>,
    pub processes: IndexMap<String, ProcessModel>,
    pub functions: IndexMap<String, FunctionModel>,
    pub traits: IndexMap<String, TraitModel>,
    pub units: IndexMap<String, UnitModel>,
}

#[derive(Clone, Debug, Serialize)]
pub struct ResourceModel {
    pub path: String,
    pub capacity: String,
    pub capacity_type: Type,
}

#[derive(Clone, Debug, Serialize)]
pub struct ArtifactModel {
    pub path: String,
    pub fields: IndexMap<String, Type>,
    pub derived_fields: BTreeSet<String>,
}

#[derive(Clone, Debug, Serialize)]
pub struct MemberModel {
    pub role: MemberRole,
    /// True for `expose config` members advertised to design tooling.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub exposed: bool,
    pub ty: Type,
    pub declared_by: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub default: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub aggregate: Option<AggregateRule>,
}

#[derive(Clone, Debug, Serialize)]
pub struct ProcessModel {
    pub path: String,
    pub parents: Vec<String>,
    pub members: IndexMap<String, MemberModel>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub monitors: Vec<String>,
    #[serde(default, skip_serializing_if = "IndexMap::is_empty")]
    pub monitor_display_units: IndexMap<String, String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub approximate_monitors: Vec<String>,
    pub parameters: IndexMap<String, Type>,
    /// Effective root configuration for this compilation, including overrides.
    #[serde(default, skip_serializing_if = "IndexMap::is_empty")]
    pub default_config: IndexMap<String, String>,
    #[serde(default, skip_serializing_if = "IndexMap::is_empty")]
    pub default_config_sources: IndexMap<String, ConfigValueSource>,
    #[serde(default, skip_serializing_if = "IndexMap::is_empty")]
    pub default_configured_arguments: IndexMap<String, ConfiguredProcessModel>,
    /// Names and inferred types visible in the process body after elaboration.
    /// This is a query surface for IDE features, not a stable HIR representation.
    pub bindings: IndexMap<String, Type>,
    pub binding_declarations: IndexMap<String, usize>,
    pub bodyless: bool,
    pub composite: bool,
    pub runs: IndexMap<String, RunModel>,
    pub wires: Vec<WireModel>,
    pub ordering: Vec<(String, String)>,
    /// Direct aggregate-export definitions, before child totals are folded in.
    #[serde(default, skip_serializing_if = "IndexMap::is_empty")]
    pub aggregate_contributions: IndexMap<String, String>,
}

#[derive(Clone, Debug, Serialize)]
pub struct RunModel {
    pub process: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub process_source: Option<String>,
    pub configured_by: String,
    pub config: IndexMap<String, String>,
    #[serde(default, skip_serializing_if = "IndexMap::is_empty")]
    pub config_sources: IndexMap<String, ConfigValueSource>,
    #[serde(default, skip_serializing_if = "IndexMap::is_empty")]
    pub configured_arguments: IndexMap<String, ConfiguredProcessModel>,
    pub arguments: IndexMap<String, String>,
}

#[derive(Clone, Debug, Serialize)]
pub struct ConfiguredProcessModel {
    pub process: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub process_source: Option<String>,
    pub config: IndexMap<String, String>,
    #[serde(default, skip_serializing_if = "IndexMap::is_empty")]
    pub config_sources: IndexMap<String, ConfigValueSource>,
    #[serde(default, skip_serializing_if = "IndexMap::is_empty")]
    pub configured_arguments: IndexMap<String, ConfiguredProcessModel>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ConfigValueSource {
    Supplied,
    Defaulted,
    Overridden,
}

#[derive(Clone, Debug, Serialize)]
pub struct WireModel {
    pub source: String,
    pub target: String,
    pub ty: String,
    pub ownership: String,
}

#[derive(Clone, Debug, Serialize)]
pub struct FunctionModel {
    pub path: String,
    pub generics: Vec<String>,
    pub parameters: IndexMap<String, Type>,
    pub outputs: IndexMap<String, Type>,
    pub bounds: Vec<String>,
}

#[derive(Clone, Debug, Serialize)]
pub struct TraitModel {
    pub path: String,
    pub generics: Vec<String>,
    pub functions: Vec<String>,
}

#[derive(Clone, Debug, Serialize)]
pub struct UnitModel {
    pub path: String,
    pub dimension: String,
    pub scale: String,
    /// Additive term of an affine unit's conversion, omitted for the ordinary
    /// multiplicative case. Without it a declared alias of an absolute scale
    /// would be reported as if it converted by scale alone.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub offset: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub refinements: Vec<String>,
}

#[derive(Clone, Debug)]
pub struct Compilation {
    pub report: Report,
    pub model: SemanticModel,
    pub source_text: IndexMap<String, String>,
    /// Raw active override text, keyed by its user-facing semantic path.
    pub overrides: IndexMap<String, String>,
    /// Every unit in scope, built-ins included, after the model's own `unit`
    /// declarations have been applied. Consumers that need to name a dimension
    /// or convert a value should read this rather than re-deriving a table.
    pub registry: TypeRegistry,
}

impl Compilation {
    /// Present internal module keys through their source declaration paths.
    /// Symbolic analysis retains identifier-safe keys for exact reparsing.
    pub(crate) fn display_text(&self, value: &str) -> String {
        if !value.contains("__module_") {
            return value.to_owned();
        }
        let names = self
            .model
            .units
            .iter()
            .map(|(key, value)| (key, &value.path))
            .chain(
                self.model
                    .artifacts
                    .iter()
                    .map(|(key, value)| (key, &value.path)),
            )
            .chain(
                self.model
                    .resources
                    .iter()
                    .map(|(key, value)| (key, &value.path)),
            )
            .chain(
                self.model
                    .processes
                    .iter()
                    .map(|(key, value)| (key, &value.path)),
            )
            .chain(
                self.model
                    .functions
                    .iter()
                    .map(|(key, value)| (key, &value.path)),
            )
            .chain(
                self.model
                    .traits
                    .iter()
                    .map(|(key, value)| (key, &value.path)),
            );
        let mut text = value.to_owned();
        for (key, path) in names {
            if key.contains("__module_") {
                text = text.replace(key, path);
            }
        }
        text
    }

    pub fn render_human(&self) -> String {
        let mut output = String::new();
        for diagnostic in &self.report.diagnostics {
            let severity = match diagnostic.severity {
                Severity::Error => "error",
                Severity::Warning => "warning",
            };
            output.push_str(&format!(
                "{severity}[{}]: {}\n  --> {}:{}:{}\n",
                diagnostic.code,
                diagnostic.message,
                diagnostic.span.path,
                diagnostic.span.line,
                diagnostic.span.column
            ));
            for note in &diagnostic.notes {
                output.push_str(&format!("  = note: {note}\n"));
            }
        }
        output.push_str(&format!(
            "status: {}\n",
            match self.report.status {
                Completeness::Complete => "complete",
                Completeness::Partial => "partial",
                Completeness::Invalid => "invalid",
            }
        ));
        output.push_str(&format!("modules: {}\n", self.report.modules.len()));
        output.push_str(&format!("obligations: {}\n", self.report.obligations.len()));
        for obligation in &self.report.obligations {
            output.push_str(&format!(
                "  - {}: {} ({})\n",
                obligation.path, obligation.expected, obligation.kind
            ));
        }
        output.push_str(&format!(
            "monitored metrics: {}\n",
            self.report.monitored_metrics.len()
        ));
        for metric in &self.report.monitored_metrics {
            if let Some(temporal) = self
                .report
                .temporal_metrics
                .iter()
                .find(|temporal| temporal.path == metric.path)
                .filter(|temporal| temporal.status == "incomplete")
            {
                output.push_str(&format!(
                    "  {}:\n    known work: {}\n    successful completion: unresolved ({} open duration obligation{})\n",
                    metric.path,
                    format_quantity_range(&temporal.known_work.lower, &temporal.known_work.upper),
                    temporal.obligations.len(),
                    if temporal.obligations.len() == 1 { "" } else { "s" },
                ));
                continue;
            }
            if let Some(bounds) = &metric.bounds {
                output.push_str(&format!(
                    "  {}:\n    {}\n    {}\n",
                    metric.path,
                    format_metric_bound_range(bounds),
                    bounds.reason
                ));
                continue;
            }
            let direct = metric
                .direct_value
                .as_ref()
                .map(|value| format!(", self = {value}"))
                .unwrap_or_default();
            output.push_str(&format!(
                "  - {}: {} = {}{} ({})\n",
                metric.path,
                metric.ty,
                metric.display_value(),
                direct,
                metric.aggregate
            ));
        }
        output.push_str(&format!(
            "resource schedules: {}\n",
            self.report.resource_schedules.len()
        ));
        for schedule in &self.report.resource_schedules {
            output.push_str(&format!(
                "  - {}: {} jobs × {} repeats, {} operations, makespan {}\n",
                schedule.path,
                schedule.jobs,
                schedule.repetitions,
                schedule.operation_count,
                schedule.makespan
            ));
            for resource in &schedule.resources {
                output.push_str(&format!(
                    "      {} via {}: {} parallel, stage utilization {}\n",
                    resource.path, resource.stage, resource.parallel_capacity, resource.utilization
                ));
            }
        }
        output.push_str(&format!(
            "provisioned resources: {}\n",
            self.report.provisioned_resources.len()
        ));
        for resource in &self.report.provisioned_resources {
            output.push_str(&format!(
                "  - {}: {} capacity {}, aggregate utilization {}\n",
                resource.path,
                resource.resource,
                resource.capacity,
                resource.utilization.as_deref().unwrap_or("unresolved")
            ));
        }
        output.push_str(&format!(
            "residual constraints: {}\n",
            self.report.constraints.len()
        ));
        for constraint in &self.report.constraints {
            output.push_str(&format!(
                "  - [{}] {}: {}\n",
                constraint.phase, constraint.path, constraint.predicate
            ));
        }
        self.display_text(&output)
    }
}

#[derive(Clone, Debug)]
pub struct Compiler {
    options: CompilerOptions,
}

impl Compiler {
    pub fn new(options: CompilerOptions) -> Self {
        Self { options }
    }

    pub fn check_path(&self, entry: &Path) -> Result<Compilation, CompileError> {
        let sources = load(entry, self.options.absolute_paths)?;
        Analyzer::new(sources, self.options.overrides.clone()).analyze()
    }

    pub fn check_path_with_overlays(
        &self,
        entry: &Path,
        overlays: &HashMap<PathBuf, String>,
    ) -> Result<Compilation, CompileError> {
        let sources = load_with_overlays(entry, self.options.absolute_paths, overlays)?;
        Analyzer::new(sources, self.options.overrides.clone()).analyze()
    }

    pub(crate) fn check_sources(&self, sources: SourceSet) -> Result<Compilation, CompileError> {
        Analyzer::new(sources, self.options.overrides.clone()).analyze()
    }

    pub fn inspect_path(&self, entry: &Path, stage: InspectStage) -> Result<String, CompileError> {
        let sources = load(entry, self.options.absolute_paths)?;
        let entry_file = sources
            .files
            .get(&sources.entry_key)
            .ok_or(CompileError::MissingEntry)?;
        match stage {
            InspectStage::Cst => Ok(format!("{:#?}", entry_file.parse.cst)),
            InspectStage::Ast => Ok(serde_json::to_string_pretty(&entry_file.parse.module)?),
            InspectStage::Hir | InspectStage::Flow => {
                let compilation =
                    Analyzer::new(sources, self.options.overrides.clone()).analyze()?;
                Ok(serde_json::to_string_pretty(&compilation.model)?)
            }
            InspectStage::Constraints => {
                let compilation =
                    Analyzer::new(sources, self.options.overrides.clone()).analyze()?;
                Ok(serde_json::to_string_pretty(
                    &compilation.report.constraints,
                )?)
            }
            InspectStage::Temporal => {
                let compilation =
                    Analyzer::new(sources, self.options.overrides.clone()).analyze()?;
                Ok(serde_json::to_string_pretty(
                    &compilation.report.temporal_metrics,
                )?)
            }
        }
    }
}

#[derive(Clone)]
struct ItemRef {
    file_key: String,
    display_path: String,
    item: Item,
    semantic_path: String,
}

#[derive(Clone, Debug)]
struct ResolvedMember {
    role: MemberRole,
    exposed: bool,
    ty: Type,
    declared_by: String,
    default: Option<Expr>,
    aggregate: Option<AggregateRule>,
    span: Span,
    refinement_signature: Vec<String>,
}

#[derive(Clone, Debug)]
struct MetricCoverageGap {
    run: String,
    metric: String,
    expected: Type,
    process: String,
    rule: AggregateRule,
    span: Span,
}

#[derive(Clone, Debug)]
struct QuantityBounds {
    lower: BigRational,
    upper: BigRational,
    dimension: Dimension,
    preferred_unit: Option<String>,
    unresolved_makespan: bool,
}

#[derive(Clone, Debug, Default)]
struct ProcessSummary {
    template_constraints: Vec<TemplateConstraint>,
    yields: IndexMap<String, Vec<YieldTemplate>>,
    stream_first_emissions: IndexMap<String, StreamEmissionTemplate>,
    runs: IndexMap<String, RunTemplate>,
    wires: Vec<(Expr, Expr, Span)>,
    ordering: Vec<(String, String)>,
    // Kept empty while schema-v1 report readers transition to schedules
    // projected from `effects`. No parser construct populates this field.
    schedules: Vec<ScheduleTemplate>,
    /// Compact executable structure for graph effects inside finite loops.
    /// The ordinary run/wire maps remain the lexical graph projection; this
    /// tree retains the nesting needed by ownership and scheduling.
    effects: Vec<EffectTemplate>,
    effect_binding_types: IndexMap<String, Type>,
    effect_mutable_bindings: BTreeSet<String>,
    consumed_run_exports: BTreeSet<String>,
    stream_cardinalities: IndexMap<String, Expr>,
    assigned_exports: BTreeSet<String>,
    direct_export_values: IndexMap<String, Expr>,
    export_values: IndexMap<String, Expr>,
    surviving_inputs: BTreeSet<String>,
    mutations: Vec<MutationTemplate>,
    metric_coverage_gaps: Vec<MetricCoverageGap>,
    /// Resource pools physically provisioned by direct export assignments.
    /// Re-exports remain ordinary wires and retain the original pool identity.
    provisioned_resources: IndexMap<String, Expr>,
}

#[derive(Clone, Debug)]
enum EffectTemplate {
    Run {
        name: String,
        span: Span,
    },
    Wire {
        source: Expr,
        target: Expr,
        span: Span,
    },
    Assign {
        target: Expr,
        value: Expr,
        span: Span,
    },
    Bind {
        name: String,
        value: Expr,
        span: Span,
    },
    Yield {
        target: Expr,
        value: Expr,
        after: Option<Expr>,
        span: Span,
    },
    For {
        binding: String,
        iterable: Expr,
        body: Vec<EffectTemplate>,
        span: Span,
    },
    Repeat {
        count: Expr,
        body: Vec<EffectTemplate>,
        span: Span,
    },
    Branch {
        condition: Expr,
        then_body: Vec<EffectTemplate>,
        else_body: Vec<EffectTemplate>,
        span: Span,
    },
    Before {
        first: String,
        second: String,
        span: Span,
    },
}

#[derive(Clone, Debug)]
struct ScheduleTemplate {
    jobs: Expr,
    repetitions: Expr,
    stages: Vec<String>,
    ordering: Vec<(String, String)>,
    span: Span,
}

#[derive(Clone, Debug)]
struct TemplateConstraint {
    expression: Expr,
    phase: String,
    stream_binding: Option<(String, String)>,
    span: Span,
}

#[derive(Clone, Debug)]
struct YieldTemplate {
    #[allow(dead_code)]
    artifact: String,
    fields: IndexMap<String, Expr>,
    #[allow(dead_code)]
    span: Span,
}

#[derive(Clone, Debug)]
struct StreamEmissionTemplate {
    after: Expr,
    span: Span,
}

#[derive(Clone, Debug)]
struct ConfiguredTemplate {
    process: String,
    process_source: Option<String>,
    config: IndexMap<String, Expr>,
    config_sources: IndexMap<String, ConfigValueSource>,
    configured_arguments: IndexMap<String, ConfiguredTemplate>,
}

#[derive(Clone, Debug)]
struct RunTemplate {
    process: String,
    process_source: Option<String>,
    configured_by: String,
    config: IndexMap<String, Expr>,
    config_sources: IndexMap<String, ConfigValueSource>,
    configured_arguments: IndexMap<String, ConfiguredTemplate>,
    arguments: IndexMap<String, Expr>,
    span: Span,
}

#[derive(Clone, Debug)]
struct MutationTemplate {
    artifact: String,
    field: String,
    before: String,
    after: String,
    span: Span,
}

#[derive(Clone, Debug)]
struct ConcreteScheduleResource {
    path: String,
    capacity: BigRational,
    claim: BigRational,
    parallel_capacity: u64,
}

#[derive(Clone, Debug)]
struct ConcreteScheduleStage {
    name: String,
    duration: BigRational,
    job_durations: Option<Vec<BigRational>>,
    duration_display: String,
    parallel_capacity: u64,
    resources: Vec<ConcreteScheduleResource>,
}

fn schedule_stage_duration_bounds(stage: &ConcreteScheduleStage) -> (BigRational, BigRational) {
    let Some(durations) = stage
        .job_durations
        .as_ref()
        .filter(|values| !values.is_empty())
    else {
        return (stage.duration.clone(), stage.duration.clone());
    };
    (
        durations.iter().min().cloned().unwrap(),
        durations.iter().max().cloned().unwrap(),
    )
}

#[derive(Clone, Debug)]
struct ConcreteScheduleOperation {
    stage: usize,
    dependencies: Vec<ScheduleDependency>,
}

#[derive(Clone, Debug)]
struct SymbolicScheduleOperation {
    run: String,
    dependencies: Vec<ScheduleDependency>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum ScheduleDependency {
    Completion(usize),
    Handoff {
        operation: usize,
        delay: BigRational,
    },
    Close(usize),
}

#[derive(Clone, Debug)]
struct ScheduledStageResult {
    operation_count: u64,
    earliest_ready: BigRational,
    earliest_start: BigRational,
    latest_finish: BigRational,
    occupied_duration: BigRational,
}

#[derive(Clone, Debug)]
struct ScheduledOperationInterval {
    stage: usize,
    ready: BigRational,
    start: BigRational,
    active_finish: BigRational,
    finish: BigRational,
}

#[derive(Clone, Debug)]
struct ScheduledJobsResult {
    makespan: BigRational,
    stages: Vec<ScheduledStageResult>,
    intervals: Vec<ScheduledOperationInterval>,
    complete: bool,
}

#[derive(Clone, Debug)]
struct ProvisionedResourceDraft {
    path: String,
    resource: String,
    capacity: Expr,
    capacity_type: Type,
    available_at: Option<BigRational>,
    file_key: String,
    span: Span,
}

#[derive(Clone, Debug)]
struct ResourceWireDraft {
    source: String,
    target: String,
    target_role: Option<MemberRole>,
    target_claim: Option<(BigRational, Dimension)>,
    file_key: String,
    span: Span,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum LoopCardinality {
    Zero,
    One,
    ManyOrUnknown,
}

#[derive(Clone, Copy)]
enum FunctionContext {
    TopLevel,
    Trait,
    Impl,
}

#[derive(Clone, Default)]
struct PureAssignments {
    definite: BTreeSet<String>,
    maybe: BTreeSet<String>,
}

#[derive(Clone, Debug)]
struct MonitorPresentation {
    display_unit: Option<String>,
    approximate: bool,
}

struct Analyzer {
    sources: SourceSet,
    override_inputs: Vec<ConfigOverride>,
    resolved_overrides: IndexMap<String, IndexMap<String, Expr>>,
    active_overrides: IndexMap<String, String>,
    items: Vec<ItemRef>,
    artifacts: IndexMap<String, ItemRef>,
    resources: IndexMap<String, ItemRef>,
    processes: IndexMap<String, ItemRef>,
    functions: IndexMap<String, ItemRef>,
    traits: IndexMap<String, ItemRef>,
    registry: TypeRegistry,
    resolved_members: IndexMap<String, IndexMap<String, ResolvedMember>>,
    /// Effective `monitor` clauses per process, unioned across the extends
    /// chain. `monitor` sits beside the inheritable member declarations, so it
    /// inherits like them; a local clause overrides an inherited one.
    resolved_monitors: IndexMap<String, IndexMap<String, MonitorPresentation>>,
    summaries: IndexMap<String, ProcessSummary>,
    /// Symbolic named outputs for nonrecursive pure functions. These templates
    /// let exact checking and reporting evaluate ordinary helper calls without
    /// turning functions into runtime-only opaque expressions.
    function_output_expressions: IndexMap<String, IndexMap<String, Expr>>,
    /// Every instantiated resource endpoint mapped to its originating pool.
    resource_pool_bindings: HashMap<String, (String, BigRational, Dimension, Option<BigRational>)>,
    report: Report,
    model: SemanticModel,
}

impl Analyzer {
    fn new(sources: SourceSet, override_inputs: Vec<ConfigOverride>) -> Self {
        let entry = sources
            .files
            .get(&sources.entry_key)
            .map(|file| file.display_path.clone())
            .unwrap_or_default();
        let modules = sources
            .files
            .values()
            .map(|file| file.display_path.clone())
            .collect();
        Self {
            sources,
            override_inputs,
            resolved_overrides: IndexMap::new(),
            active_overrides: IndexMap::new(),
            items: Vec::new(),
            artifacts: IndexMap::new(),
            resources: IndexMap::new(),
            processes: IndexMap::new(),
            functions: IndexMap::new(),
            traits: IndexMap::new(),
            registry: TypeRegistry::default(),
            resolved_members: IndexMap::new(),
            resolved_monitors: IndexMap::new(),
            summaries: IndexMap::new(),
            function_output_expressions: IndexMap::new(),
            resource_pool_bindings: HashMap::new(),
            report: Report {
                schema_version: 1,
                status: Completeness::Complete,
                entry,
                modules,
                diagnostics: Vec::new(),
                symbols: Vec::new(),
                obligations: Vec::new(),
                constraints: Vec::new(),
                provenance: Vec::new(),
                assumed_zero: Vec::new(),
                monitored_metrics: Vec::new(),
                temporal_metrics: Vec::new(),
                stream_handoffs: Vec::new(),
                stream_cardinalities: Vec::new(),
                resource_schedules: Vec::new(),
                provisioned_resources: Vec::new(),
            },
            model: SemanticModel::default(),
        }
    }

    fn analyze(mut self) -> Result<Compilation, CompileError> {
        self.collect_parse_diagnostics();
        for (file, span, message) in crate::namespaces::bind(&mut self.sources) {
            self.push_error(&file, span, "E0105", message);
        }
        self.collect_items();
        self.validate_imports();
        self.resolve_units();
        self.lower_numeric_refinements();
        self.resolve_resources();
        self.resolve_artifacts();
        self.resolve_traits_and_functions();
        self.validate_refinement_declaration_contexts();
        self.resolve_process_interfaces();
        self.analyze_process_bodies();
        self.validate_process_instantiation_cycles();
        self.validate_loan_scheduling();
        self.instantiate_child_constraints();
        self.project_temporal_metrics();
        self.project_resource_schedules();
        self.attach_provisioned_resource_utilization();
        self.report.recompute_status();
        let source_text = self
            .sources
            .files
            .values()
            .map(|file| (file.display_path.clone(), file.text.clone()))
            .collect();
        Ok(Compilation {
            report: self.report,
            model: self.model,
            source_text,
            overrides: self.active_overrides,
            registry: self.registry,
        })
    }

    fn collect_parse_diagnostics(&mut self) {
        let files = self
            .sources
            .files
            .values()
            .map(|file| {
                (
                    file.key.clone(),
                    file.parse.errors.clone(),
                    file.display_path.clone(),
                )
            })
            .collect::<Vec<_>>();
        for (file_key, errors, display_path) in files {
            for error in errors {
                let span = self.span(&file_key, error.span.clone());
                self.report.diagnostics.push(Diagnostic::error(
                    format!("parse:{}:{}", display_path, error.span.start),
                    "E0001",
                    error.message,
                    span,
                ));
            }
        }
    }

    fn collect_items(&mut self) {
        let files = self
            .sources
            .files
            .values()
            .map(|file| {
                (
                    file.key.clone(),
                    file.display_path.clone(),
                    file.parse.module.items.clone(),
                )
            })
            .collect::<Vec<_>>();
        for (file_key, display_path, items) in files {
            let mut local_names = HashSet::new();
            for item in items {
                if matches!(item, Item::Impl(_)) {
                    let semantic_path = format!(
                        "{}::impl@{}",
                        display_path.trim_matches(['<', '>']),
                        item.span().start
                    );
                    let item_ref = ItemRef {
                        file_key: file_key.clone(),
                        display_path: display_path.clone(),
                        item: item.clone(),
                        semantic_path: semantic_path.clone(),
                    };
                    self.report.symbols.push(SymbolReport {
                        path: semantic_path,
                        kind: "impl".into(),
                        span: self.span(&file_key, item.span().clone()),
                    });
                    self.items.push(item_ref);
                    continue;
                }
                let Some(name) = item.name().map(str::to_owned) else {
                    continue;
                };
                let source_name = name.split("__module_").next().unwrap_or(&name);
                let semantic_path =
                    format!("{}::{source_name}", display_path.trim_matches(['<', '>']));
                let item_ref = ItemRef {
                    file_key: file_key.clone(),
                    display_path: display_path.clone(),
                    item: item.clone(),
                    semantic_path: semantic_path.clone(),
                };
                if !local_names.insert(name.clone()) {
                    self.push_error(
                        &file_key,
                        item.span().clone(),
                        "E0101",
                        format!("duplicate declaration {name}"),
                    );
                    continue;
                }
                let kind = match &item {
                    Item::Artifact(_) => {
                        self.insert_global("artifact", &name, item_ref.clone());
                        self.artifacts
                            .entry(name.clone())
                            .or_insert(item_ref.clone());
                        "artifact"
                    }
                    Item::Resource(_) => {
                        self.insert_global("resource", &name, item_ref.clone());
                        self.resources
                            .entry(name.clone())
                            .or_insert(item_ref.clone());
                        "resource"
                    }
                    Item::Process(_) => {
                        self.insert_global("process", &name, item_ref.clone());
                        self.processes
                            .entry(name.clone())
                            .or_insert(item_ref.clone());
                        "process"
                    }
                    Item::Function(_) => {
                        self.insert_global("function", &name, item_ref.clone());
                        self.functions
                            .entry(name.clone())
                            .or_insert(item_ref.clone());
                        "function"
                    }
                    Item::Trait(_) => {
                        self.insert_global("trait", &name, item_ref.clone());
                        self.traits.entry(name.clone()).or_insert(item_ref.clone());
                        "trait"
                    }
                    Item::Unit(_) => "unit",
                    Item::Impl(_) => "impl",
                    Item::Import(_) => continue,
                };
                self.report.symbols.push(SymbolReport {
                    path: semantic_path,
                    kind: kind.into(),
                    span: self.span(&file_key, item.span().clone()),
                });
                self.style_check(&item_ref, &name, kind);
                self.items.push(item_ref);
            }
        }
    }

    fn validate_imports(&mut self) {
        let files = self
            .sources
            .files
            .values()
            .map(|file| (file.key.clone(), file.parse.module.items.clone()))
            .collect::<Vec<_>>();
        for (file_key, items) in files {
            for item in items {
                let Item::Import(import) = item else {
                    continue;
                };
                let target_key = match &import.path {
                    ImportPath::Relative(relative) => self.sources.files[&file_key]
                        .path
                        .as_ref()
                        .and_then(|path| path.parent())
                        .map(|parent| parent.join(relative))
                        .and_then(|path| crate::source::source_path(&path).ok())
                        .map(|path| path.to_string_lossy().into_owned()),
                    ImportPath::Module(module) if module == "math" => None,
                    ImportPath::Module(module) if module.starts_with("std.") => {
                        Some(format!("std:{module}"))
                    }
                    ImportPath::Module(module) => {
                        self.push_error(
                            &file_key,
                            import.span.clone(),
                            "E0103",
                            format!("unknown native module {module}"),
                        );
                        None
                    }
                };
                let Some(target_key) = target_key else {
                    continue;
                };
                let Some(target) = self.sources.files.get(&target_key) else {
                    continue;
                };
                let target_display_path = target.display_path.clone();
                let available = target
                    .parse
                    .module
                    .items
                    .iter()
                    .filter_map(Item::name)
                    .map(str::to_owned)
                    .collect::<HashSet<_>>();
                for name in &import.names {
                    if !available.contains(name) {
                        self.push_error(
                            &file_key,
                            import.span.clone(),
                            "E0104",
                            format!("module {target_display_path} does not export {name}"),
                        );
                    }
                }
            }
        }
    }

    fn is_visible(&self, file_key: &str, name: &str) -> bool {
        let Some(file) = self.sources.files.get(file_key) else {
            return false;
        };
        if file
            .parse
            .module
            .items
            .iter()
            .any(|item| item.name() == Some(name))
        {
            return true;
        }
        file.parse.module.items.iter().any(|item| {
            matches!(item, Item::Import(import) if import.names.iter().any(|imported| imported == name))
        })
    }

    fn process_conforms_to(&self, implementation: &str, contract: &str) -> bool {
        fn visit(
            processes: &IndexMap<String, ItemRef>,
            current: &str,
            contract: &str,
            visited: &mut HashSet<String>,
        ) -> bool {
            if current == contract {
                return true;
            }
            if !visited.insert(current.to_owned()) {
                return false;
            }
            let Some(ItemRef {
                item: Item::Process(process),
                ..
            }) = processes.get(current)
            else {
                return false;
            };
            process
                .parents
                .iter()
                .any(|parent| visit(processes, parent, contract, visited))
        }

        visit(
            &self.processes,
            implementation,
            contract,
            &mut HashSet::new(),
        )
    }

    fn types_compatible(&self, expected: &Type, actual: &Type) -> bool {
        match (expected, actual) {
            (Type::ConfiguredProcess(contract), Type::ConfiguredProcess(implementation)) => {
                self.process_conforms_to(implementation, contract)
            }
            _ => compatible(expected, actual),
        }
    }

    fn imports_module(&self, file_key: &str, module: &str) -> bool {
        self.sources.files.get(file_key).is_some_and(|file| {
            file.parse.module.items.iter().any(|item| {
                matches!(item, Item::Import(import) if matches!(&import.path, ImportPath::Module(imported) if imported == module))
            })
        })
    }

    fn insert_global(&mut self, kind: &str, name: &str, item: ItemRef) {
        let existing = match kind {
            "artifact" => self.artifacts.get(name),
            "resource" => self.resources.get(name),
            "process" => self.processes.get(name),
            "function" => self.functions.get(name),
            "trait" => self.traits.get(name),
            _ => None,
        };
        if let Some(existing) = existing {
            self.push_error(
                &item.file_key,
                item.item.span().clone(),
                "E0102",
                format!(
                    "ambiguous {kind} name {name}; already declared in {}",
                    existing.display_path
                ),
            );
        }
    }

    fn style_check(&mut self, item: &ItemRef, name: &str, kind: &str) {
        let name = name.split("__module_").next().unwrap_or(name);
        let expects_upper = matches!(kind, "artifact" | "resource" | "process" | "trait" | "unit");
        let valid = if expects_upper {
            name.chars().next().is_some_and(char::is_uppercase)
        } else {
            is_snake_case(name)
        };
        if !valid {
            self.push_warning(
                &item.file_key,
                item.item.span().clone(),
                "W0101",
                format!("{kind} name {name} does not follow the preferred casing convention"),
            );
        }
    }

    /// Message for a type annotation that did not resolve. When the failure
    /// came from the inline unit expression itself — an unknown unit name, or
    /// an absolute unit used inside a compound — the registry's reason is far
    /// more useful than "unknown type".
    fn unknown_type_message(&self, prefix: &str, ty: &TypeExpr) -> String {
        if ty.fields.is_some() {
            return format!(
                "{prefix} {ty}: named tuple fields must resolve to copyable types; artifacts, streams, resources and process values cannot be contained"
            );
        }
        self.registry.inline_unit_error(ty).map_or_else(
            || format!("{prefix} {ty}"),
            |reason| format!("{prefix} {ty}: {reason}"),
        )
    }

    fn resolve_units(&mut self) {
        let units = self
            .items
            .iter()
            .filter_map(|item| match &item.item {
                Item::Unit(unit) => Some((item.clone(), unit.clone())),
                _ => None,
            })
            .collect::<Vec<_>>();
        for (item, unit) in units {
            // An explicit unit declaration takes precedence over a built-in.
            // Repeated user declarations across loaded modules remain errors.
            if self.model.units.contains_key(&unit.name) {
                self.push_error(
                    &item.file_key,
                    unit.span,
                    "E0201",
                    format!("unit {} is already declared in this model", unit.name),
                );
                continue;
            }
            if let Some(body) = &unit.refinement {
                if matches!(
                    unit.name.as_str(),
                    "Scalar" | "Integer" | "Bool" | "String" | "Stream" | "Configured"
                ) {
                    self.push_error(
                        &item.file_key,
                        unit.span,
                        "E0203",
                        "a refinement must use a new name instead of replacing a primitive type",
                    );
                    continue;
                }
                let ExprKind::Call { callee, arguments } = &unit.value.kind else {
                    self.push_error(
                        &item.file_key,
                        unit.span,
                        "E0203",
                        "numeric refinement expects a base and binder, such as Scalar(x)",
                    );
                    continue;
                };
                let Some(base_name) = callee.as_name() else {
                    self.push_error(
                        &item.file_key,
                        unit.span,
                        "E0203",
                        "refinement base must be a numeric type name",
                    );
                    continue;
                };
                let binder = arguments.first().and_then(|arg| arg.value.as_name());
                if arguments.len() != 1 || arguments[0].name.is_some() || binder.is_none() {
                    self.push_error(
                        &item.file_key,
                        unit.span,
                        "E0203",
                        "numeric refinement requires exactly one bare binder name",
                    );
                    continue;
                }
                let binder = binder.unwrap().to_owned();
                let base_expr = TypeExpr {
                    fields: None,
                    dimension: None,
                    name: base_name.into(),
                    arguments: vec![],
                    arrays: vec![],
                    span: callee.span.clone(),
                };
                let base = self
                    .registry
                    .resolve_type(&base_expr, &self.artifact_name_map(), &[]);
                if base.dimension().is_none() {
                    self.push_error(
                        &item.file_key,
                        unit.span,
                        "E0203",
                        format!(
                            "refinement base {base_name} must be an already defined numeric type"
                        ),
                    );
                    continue;
                }
                let mut predicates = self
                    .registry
                    .refinements
                    .get(base_name)
                    .map(|parent| {
                        let substitutions = IndexMap::from([(
                            parent.binder.clone(),
                            Expr::name(&binder, unit.span.clone()),
                        )]);
                        parent
                            .predicates
                            .iter()
                            .map(|predicate| normalize_expr(predicate, &substitutions))
                            .collect::<Vec<_>>()
                    })
                    .unwrap_or_default();
                if base == Type::Integer && !self.registry.refinements.contains_key(base_name) {
                    predicates.push(Expr {
                        kind: ExprKind::Call {
                            callee: Box::new(Expr::name("is_integer", unit.span.clone())),
                            arguments: vec![CallArgument {
                                name: None,
                                value: Expr::name(&binder, unit.span.clone()),
                                span: unit.span.clone(),
                            }],
                        },
                        span: unit.span.clone(),
                    });
                }
                let mut env = TypeEnv::default();
                env.values.insert(binder.clone(), base.clone());
                let mut valid = true;
                for statement in &body.statements {
                    let Stmt::Requires { value, span } = statement else {
                        self.push_error(
                            &item.file_key,
                            statement.span().clone(),
                            "E0203",
                            "refinement bodies contain only requires predicates",
                        );
                        valid = false;
                        continue;
                    };
                    if self.infer_expr(&item.file_key, value, &env) != Type::Bool {
                        self.push_error(
                            &item.file_key,
                            span.clone(),
                            "E0203",
                            "refinement predicate must be Bool",
                        );
                        valid = false;
                    }
                    predicates.push(value.clone());
                }
                if valid {
                    self.model.units.insert(
                        unit.name.clone(),
                        UnitModel {
                            path: item.semantic_path,
                            dimension: base.dimension().unwrap().to_string(),
                            scale: base
                                .unit_scale()
                                .map(|v| format!("{}/{}", v.numer(), v.denom()))
                                .unwrap_or_else(|| "1/1".into()),
                            offset: None,
                            refinements: predicates.iter().map(Expr::display).collect(),
                        },
                    );
                    self.registry.refinements.insert(
                        unit.name,
                        NumericRefinement {
                            base,
                            binder,
                            predicates,
                        },
                    );
                }
                continue;
            }
            match self.registry.define_unit(&unit.name, &unit.value) {
                Ok(resolved) => {
                    self.model.units.insert(
                        unit.name.clone(),
                        UnitModel {
                            path: item.semantic_path,
                            dimension: resolved.dimension.to_string(),
                            scale: format!("{}/{}", resolved.scale.numer(), resolved.scale.denom()),
                            offset: resolved.is_absolute().then(|| {
                                format!("{}/{}", resolved.offset.numer(), resolved.offset.denom())
                            }),
                            refinements: vec![],
                        },
                    );
                }
                Err(message) => self.push_error(&item.file_key, unit.span, "E0202", message),
            }
        }
    }

    /// Refinements are predicates on numeric values, not nominal arithmetic
    /// types. Lower their uses before ordinary checking so constraints follow
    /// the same branch, loop and instance substitution rules as `requires`.
    fn refinement_guards(&self, ty: &TypeExpr, value: &Expr) -> Vec<Stmt> {
        self.refinement_guards_inner(ty, value, &mut HashSet::new())
    }

    fn refinement_guards_inner(
        &self,
        ty: &TypeExpr,
        value: &Expr,
        seen: &mut HashSet<String>,
    ) -> Vec<Stmt> {
        if !ty.arrays.is_empty() {
            let mut element = ty.clone();
            element.arrays.pop();
            let binder = format!("__refinement_item_{}", ty.arrays.len());
            return self
                .refinement_guards_inner(&element, &Expr::name(&binder, value.span.clone()), seen)
                .into_iter()
                .map(|statement| {
                    let Stmt::Requires {
                        value: predicate, ..
                    } = statement
                    else {
                        unreachable!()
                    };
                    Stmt::Requires {
                        value: quantify_over(value, &binder, &predicate, &value.span),
                        span: value.span.clone(),
                    }
                })
                .collect();
        }
        if let Some(fields) = &ty.fields {
            return fields
                .iter()
                .flat_map(|field| {
                    let field_value = Expr {
                        kind: ExprKind::Member {
                            value: Box::new(value.clone()),
                            member: field.name.clone(),
                        },
                        span: value.span.clone(),
                    };
                    self.refinement_guards_inner(&field.ty, &field_value, seen)
                })
                .collect();
        }
        if let Some(ItemRef {
            item: Item::Artifact(artifact),
            ..
        }) = self.artifacts.get(&ty.name)
        {
            if !seen.insert(ty.name.clone()) {
                return vec![];
            }
            let mut guards = vec![];
            for field in &artifact.fields {
                let field_value = Expr {
                    kind: ExprKind::Member {
                        value: Box::new(value.clone()),
                        member: field.name.clone(),
                    },
                    span: value.span.clone(),
                };
                guards.extend(self.refinement_guards_inner(&field.ty, &field_value, seen));
            }
            seen.remove(&ty.name);
            return guards;
        }
        let Some(refinement) = self.registry.refinements.get(&ty.name) else {
            return vec![];
        };
        let substitutions = IndexMap::from([(refinement.binder.clone(), value.clone())]);
        refinement
            .predicates
            .iter()
            .map(|predicate| Stmt::Requires {
                value: normalize_expr(predicate, &substitutions),
                span: value.span.clone(),
            })
            .collect()
    }

    fn process_refinement_annotations(
        &self,
        process: &Process,
        seen: &mut HashSet<String>,
    ) -> IndexMap<String, TypeExpr> {
        if !seen.insert(process.name.clone()) {
            return IndexMap::new();
        }
        let mut annotations = IndexMap::new();
        for parent in &process.parents {
            if let Some(ItemRef {
                item: Item::Process(parent),
                ..
            }) = self.processes.get(parent)
            {
                annotations.extend(self.process_refinement_annotations(parent, seen));
            }
        }
        annotations.extend(
            process
                .members
                .iter()
                .map(|member| (member.name.clone(), member.ty.clone())),
        );
        annotations.extend(
            process
                .parameters
                .iter()
                .map(|parameter| (parameter.name.clone(), parameter.ty.clone())),
        );
        annotations
    }

    fn refinement_expression_type(
        &self,
        expression: &Expr,
        annotations: &IndexMap<String, TypeExpr>,
    ) -> Option<TypeExpr> {
        match &expression.kind {
            ExprKind::Name { value } => annotations.get(value).cloned(),
            ExprKind::Call { callee, .. } => {
                let name = if callee
                    .as_name()
                    .is_some_and(|name| self.artifacts.contains_key(name))
                {
                    callee.as_name()?.to_owned()
                } else if callee
                    .as_name()
                    .is_some_and(|name| self.processes.contains_key(name))
                {
                    format!("@configured:{}", callee.as_name()?)
                } else {
                    let configured = self.refinement_expression_type(callee, annotations)?;
                    let name = configured.name.strip_prefix("@configured:")?;
                    format!("@run:{name}")
                };
                Some(TypeExpr {
                    fields: None,
                    name,
                    dimension: None,
                    arguments: vec![],
                    arrays: vec![],
                    span: expression.span.clone(),
                })
            }
            ExprKind::Member { value, member } => {
                let parent = self.refinement_expression_type(value, annotations)?;
                if let Some(fields) = &parent.fields {
                    return fields
                        .iter()
                        .find(|field| &field.name == member)
                        .map(|field| field.ty.clone());
                }
                if let Some(process) = parent.name.strip_prefix("@run:") {
                    let Item::Process(process) = &self.processes.get(process)?.item else {
                        return None;
                    };
                    return self
                        .process_refinement_annotations(process, &mut HashSet::new())
                        .get(member)
                        .cloned();
                }
                let Item::Artifact(artifact) = &self.artifacts.get(&parent.name)?.item else {
                    return None;
                };
                artifact
                    .fields
                    .iter()
                    .find(|field| &field.name == member)
                    .map(|field| field.ty.clone())
            }
            ExprKind::Index { value, .. } => {
                let mut ty = self.refinement_expression_type(value, annotations)?;
                ty.arrays.pop()?;
                Some(ty)
            }
            _ => None,
        }
    }

    fn refinement_call_guards(&self, expression: &Expr) -> Vec<Stmt> {
        self.refinement_call_guards_inner(expression, &mut HashSet::new())
    }

    fn refinement_call_guards_inner(
        &self,
        expression: &Expr,
        expanding: &mut HashSet<String>,
    ) -> Vec<Stmt> {
        let mut guards = vec![];
        visit_expr(expression, &mut |expr| {
            let ExprKind::Call { callee, arguments } = &expr.kind else {
                return;
            };
            let Some(name) = callee.as_name() else {
                return;
            };
            let parameters: Vec<(String, TypeExpr)> = if let Some(ItemRef {
                item: Item::Function(function),
                ..
            }) = self.functions.get(name)
            {
                function
                    .parameters
                    .iter()
                    .map(|p| (p.name.clone(), p.ty.clone()))
                    .collect()
            } else if let Some(ItemRef {
                item: Item::Artifact(artifact),
                ..
            }) = self.artifacts.get(name)
            {
                artifact
                    .fields
                    .iter()
                    .filter(|f| f.derived.is_none())
                    .map(|f| (f.name.clone(), f.ty.clone()))
                    .collect()
            } else if let Some(ItemRef {
                item: Item::Process(process),
                ..
            }) = self.processes.get(name)
            {
                self.process_refinement_annotations(process, &mut HashSet::new())
                    .into_iter()
                    .collect()
            } else {
                return;
            };
            for (index, argument) in arguments.iter().enumerate() {
                let parameter = if let Some(name) = &argument.name {
                    parameters.iter().find(|(parameter, _)| parameter == name)
                } else if self.processes.contains_key(name) {
                    argument
                        .value
                        .as_name()
                        .and_then(|name| parameters.iter().find(|(parameter, _)| parameter == name))
                } else {
                    parameters.get(index)
                };
                if let Some((_, ty)) = parameter {
                    guards.extend(self.refinement_guards(ty, &argument.value));
                }
            }
            if let Some(ItemRef {
                item: Item::Function(function),
                ..
            }) = self.functions.get(name)
            {
                let substitutions = arguments
                    .iter()
                    .enumerate()
                    .filter_map(|(index, argument)| {
                        argument
                            .name
                            .clone()
                            .or_else(|| parameters.get(index).map(|p| p.0.clone()))
                            .map(|name| (name, argument.value.clone()))
                    })
                    .collect();
                if let Some(body) = &function.body {
                    for predicate in symbolic_refinement_requirements(body, &mut IndexMap::new())
                        .unwrap_or_default()
                    {
                        guards.push(Stmt::Requires {
                            value: normalize_expr(&predicate, &substitutions),
                            span: expression.span.clone(),
                        });
                    }
                }
                for output in &function.outputs {
                    let value = Expr {
                        kind: ExprKind::Member {
                            value: Box::new(expr.clone()),
                            member: output.name.clone(),
                        },
                        span: expr.span.clone(),
                    };
                    guards.extend(self.refinement_guards(&output.ty, &value));
                }
            }
            // Derived constituents also have to satisfy their annotations.
            if let Some(ItemRef {
                item: Item::Artifact(artifact),
                ..
            }) = self.artifacts.get(name)
            {
                if !expanding.insert(name.to_owned()) {
                    return;
                }
                let mut substitutions = IndexMap::new();
                for (index, argument) in arguments.iter().enumerate() {
                    if let Some(name) = argument
                        .name
                        .clone()
                        .or_else(|| parameters.get(index).map(|p| p.0.clone()))
                    {
                        substitutions.insert(name, argument.value.clone());
                    }
                }
                for field in &artifact.fields {
                    if let Some(derived) = &field.derived {
                        let value = normalize_expr(derived, &substitutions);
                        guards.extend(self.refinement_call_guards_inner(&value, expanding));
                        guards.extend(self.refinement_guards(&field.ty, &value));
                        substitutions.insert(field.name.clone(), value);
                    }
                }
                expanding.remove(name);
            }
        });
        // Expanded function/derived-field predicates may originate in another
        // file; diagnostics at this boundary belong to the calling expression.
        for guard in &mut guards {
            if let Stmt::Requires { span, .. } = guard {
                *span = expression.span.clone();
            }
        }
        guards
    }

    fn lower_refinement_block(
        &self,
        block: &mut Block,
        annotations: &mut IndexMap<String, TypeExpr>,
    ) {
        let mut statements = vec![];
        for mut statement in std::mem::take(&mut block.statements) {
            let mut guards = vec![];
            match &mut statement {
                Stmt::Let {
                    name, ty, value, ..
                } => {
                    guards.extend(self.refinement_call_guards(value));
                    if let Some(ty) = ty {
                        guards.extend(self.refinement_guards(ty, value));
                        annotations.insert(name.clone(), ty.clone());
                    } else if let Some(ty) = self.refinement_expression_type(value, annotations) {
                        annotations.insert(name.clone(), ty);
                    } else {
                        annotations.shift_remove(name);
                    }
                }
                Stmt::Assign { target, value, .. } => {
                    guards.extend(self.refinement_call_guards(value));
                    if let Some(ty) = self.refinement_expression_type(target, annotations) {
                        guards.extend(self.refinement_guards(&ty, value));
                    }
                }
                Stmt::For {
                    binding,
                    iterable,
                    body,
                    ..
                } => {
                    guards.extend(self.refinement_call_guards(iterable));
                    let mut nested = annotations.clone();
                    if let Some(mut ty) = self.refinement_expression_type(iterable, annotations) {
                        if ty.arrays.pop().is_some() {
                            nested.insert(binding.clone(), ty);
                        } else if ty.name == "Stream" {
                            if let Some(TypeArgument::Type(element)) = ty.arguments.first() {
                                nested.insert(binding.clone(), element.clone());
                            }
                        }
                    }
                    self.lower_refinement_block(body, &mut nested);
                    if let Some(ty) = nested.get(binding) {
                        let mut initial = self.refinement_guards(
                            ty,
                            &Expr::name(binding.clone(), iterable.span.clone()),
                        );
                        initial.append(&mut body.statements);
                        body.statements = initial;
                    }
                }
                Stmt::Repeat { count, body, .. } => {
                    guards.extend(self.refinement_call_guards(count));
                    self.lower_refinement_block(body, &mut annotations.clone());
                }
                Stmt::If {
                    condition,
                    then_branch,
                    else_branch,
                    ..
                } => {
                    guards.extend(self.refinement_call_guards(condition));
                    self.lower_refinement_block(then_branch, &mut annotations.clone());
                    if let Some(body) = else_branch {
                        self.lower_refinement_block(body, &mut annotations.clone());
                    }
                }
                Stmt::Yield { target, value, .. } => {
                    guards.extend(self.refinement_call_guards(value));
                    if let Some(ty) = self.refinement_expression_type(target, annotations) {
                        if ty.name == "Stream" {
                            if let Some(TypeArgument::Type(element)) = ty.arguments.first() {
                                guards.extend(self.refinement_guards(element, value));
                            }
                        }
                    }
                }
                Stmt::Requires { value, .. } | Stmt::Expr { value, .. } => {
                    guards.extend(self.refinement_call_guards(value))
                }
                _ => {}
            }
            if let Stmt::Expr { value, .. } = &statement {
                if let ExprKind::Call { callee, arguments } = &value.kind {
                    if let ExprKind::Member {
                        value: collection,
                        member,
                    } = &callee.kind
                    {
                        if member == "append" && arguments.len() == 1 {
                            if let Some(mut ty) =
                                self.refinement_expression_type(collection, annotations)
                            {
                                if ty.arrays.pop().is_some() {
                                    guards.extend(self.refinement_guards(&ty, &arguments[0].value));
                                }
                            }
                        }
                    }
                }
            }
            // Check RHS values before the assignment changes the environment.
            let mut seen = HashSet::new();
            guards.retain(|statement| match statement {
                Stmt::Requires { value, .. } => seen.insert(value.display()),
                _ => true,
            });
            statements.extend(guards);
            statements.push(statement);
        }
        block.statements = statements;
    }

    fn lower_numeric_refinements(&mut self) {
        if self.registry.refinements.is_empty() {
            return;
        }
        let mut visited = HashSet::new();
        for name in self.functions.keys().cloned().collect::<Vec<_>>() {
            self.lower_function_refinements(&name, &mut visited);
        }
        for (name, mut item) in self.processes.clone() {
            let Item::Process(process) = &mut item.item else {
                continue;
            };
            let mut annotations = self.process_refinement_annotations(process, &mut HashSet::new());
            let export_names = self.process_export_names(process, &mut HashSet::new());
            let mut initial = vec![];
            for (name, ty) in &annotations {
                if !export_names.contains(name) {
                    initial.extend(
                        self.refinement_guards(ty, &Expr::name(name, process.span.clone())),
                    );
                }
            }
            if let Some(body) = &mut process.body {
                self.lower_refinement_block(body, &mut annotations);
                initial.append(&mut body.statements);
                body.statements = initial;
            }
            self.processes.insert(name, item);
        }
    }

    fn lower_function_refinements(&mut self, name: &str, visited: &mut HashSet<String>) {
        if !visited.insert(name.to_owned()) {
            return;
        }
        let Some(mut item) = self.functions.get(name).cloned() else {
            return;
        };
        let Item::Function(function) = &mut item.item else {
            return;
        };
        if let Some(body) = &function.body {
            let mut callees = vec![];
            visit_block_exprs(body, &mut |expression| {
                if let ExprKind::Call { callee, .. } = &expression.kind {
                    if let Some(name) = callee.as_name() {
                        callees.push(name.to_owned());
                    }
                }
            });
            for callee in callees {
                self.lower_function_refinements(&callee, visited);
            }
        }
        if let Some(body) = &mut function.body {
            let original_requirements = count_block_requirements(body);
            let mut annotations = function
                .parameters
                .iter()
                .chain(&function.outputs)
                .map(|p| (p.name.clone(), p.ty.clone()))
                .collect();
            self.lower_refinement_block(body, &mut annotations);
            let mut initial = function
                .parameters
                .iter()
                .flat_map(|p| self.refinement_guards(&p.ty, &Expr::name(&p.name, p.span.clone())))
                .collect::<Vec<_>>();
            initial.append(&mut body.statements);
            body.statements = initial;
            if count_block_requirements(body) > original_requirements
                && symbolic_refinement_requirements(body, &mut IndexMap::new()).is_none()
            {
                self.push_error(&item.file_key, body.span.clone(), "E0204", "refinement checking across a stateful symbolic pure-function loop is not yet supported; use a concrete finite loop or a stateless per-item helper");
            }
        }
        self.functions.insert(name.to_owned(), item);
    }

    fn process_export_names(
        &self,
        process: &Process,
        seen: &mut HashSet<String>,
    ) -> HashSet<String> {
        if !seen.insert(process.name.clone()) {
            return HashSet::new();
        }
        let mut exports = process
            .members
            .iter()
            .filter(|m| m.role == MemberRole::Export)
            .map(|m| m.name.clone())
            .collect::<HashSet<_>>();
        for parent in &process.parents {
            if let Some(ItemRef {
                item: Item::Process(parent),
                ..
            }) = self.processes.get(parent)
            {
                exports.extend(self.process_export_names(parent, seen));
            }
        }
        exports
    }

    fn type_contains_refinement(&self, ty: &TypeExpr) -> bool {
        self.registry.refinements.contains_key(&ty.name)
            || ty.fields.as_ref().is_some_and(|fields| fields.iter().any(|field| self.type_contains_refinement(&field.ty)))
            || ty.arguments.iter().any(|argument| matches!(argument, TypeArgument::Type(ty) if self.type_contains_refinement(ty)))
    }

    /// These contexts do not currently feed symbolic predicates into an
    /// instantiated process. Refuse unchecked refinements instead of quietly
    /// erasing their contracts at those boundaries.
    fn validate_refinement_declaration_contexts(&mut self) {
        if self.registry.refinements.is_empty() {
            return;
        }
        for item in self.items.clone() {
            match &item.item {
                Item::Trait(declaration) => {
                    for function in &declaration.functions {
                        if function
                            .parameters
                            .iter()
                            .chain(&function.outputs)
                            .any(|p| self.type_contains_refinement(&p.ty))
                        {
                            self.push_error(&item.file_key, function.span.clone(), "E0204", "numeric refinements in trait signatures are not yet supported; use a concrete pure function");
                        }
                    }
                }
                Item::Impl(declaration) => {
                    for function in &declaration.functions {
                        let mut refined = function
                            .parameters
                            .iter()
                            .chain(&function.outputs)
                            .any(|p| self.type_contains_refinement(&p.ty));
                        if let Some(body) = &function.body {
                            let original = count_block_requirements(body);
                            let mut lowered = body.clone();
                            self.lower_refinement_block(&mut lowered, &mut IndexMap::new());
                            refined |= count_block_requirements(&lowered) > original;
                        }
                        if refined {
                            self.push_error(&item.file_key, function.span.clone(), "E0204", "numeric refinements in trait implementations are not yet supported; use a concrete pure function");
                        }
                    }
                }
                Item::Process(process) => {
                    for member in &process.members {
                        let Some(default) = &member.default else {
                            continue;
                        };
                        let guards = self.refinement_call_guards(default);
                        if guards.iter().any(|guard| matches!(guard, Stmt::Requires { value, .. } if self.evaluate_predicate(value) != Some(true))) {
                            self.push_error(&item.file_key, default.span.clone(), "E0204", "a declaration default calls a refined function with predicates that are not statically satisfied; move the call into the process invocation body");
                        }
                    }
                }
                _ => {}
            }
        }
    }

    fn resolve_resources(&mut self) {
        let resources = self.resources.values().cloned().collect::<Vec<_>>();
        for item in resources {
            let Item::Resource(resource) = &item.item else {
                continue;
            };
            let environment = TypeEnv::default();
            let capacity_type = self.infer_expr(&item.file_key, &resource.capacity, &environment);
            let numeric = matches!(capacity_type, Type::Integer | Type::Scalar)
                || capacity_type.dimension().is_some();
            if capacity_type == Type::Error || !numeric {
                self.push_error(
                    &item.file_key,
                    resource.capacity.span.clone(),
                    "E0251",
                    format!(
                        "resource {} capacity must be a numeric scalar or quantity, found {capacity_type}",
                        resource.name
                    ),
                );
                continue;
            }
            let positive = Expr {
                kind: ExprKind::Binary {
                    op: BinaryOp::Gt,
                    left: Box::new(resource.capacity.clone()),
                    right: Box::new(
                        self.zero_expression(&capacity_type, resource.capacity.span.clone()),
                    ),
                },
                span: resource.capacity.span.clone(),
            };
            if self.evaluate_predicate(&positive) == Some(false) {
                self.push_error(
                    &item.file_key,
                    resource.capacity.span.clone(),
                    "E0252",
                    format!("resource {} capacity must be positive", resource.name),
                );
            }
            self.registry
                .define_resource(&resource.name, capacity_type.clone());
            self.model.resources.insert(
                resource.name.clone(),
                ResourceModel {
                    path: item.semantic_path.clone(),
                    capacity: resource.capacity.display(),
                    capacity_type,
                },
            );
        }
    }

    fn resolve_artifacts(&mut self) {
        let artifact_names = self.artifact_name_map();
        let artifacts = self.artifacts.values().cloned().collect::<Vec<_>>();
        for item in artifacts {
            let Item::Artifact(artifact) = &item.item else {
                continue;
            };
            let mut fields = IndexMap::new();
            let mut derived_fields = BTreeSet::new();
            for field in &artifact.fields {
                if fields.contains_key(&field.name) {
                    self.push_error(
                        &item.file_key,
                        field.span.clone(),
                        "E0301",
                        format!("duplicate artifact field {}", field.name),
                    );
                    continue;
                }
                if field.ty.name == "Configured" {
                    self.push_error(
                        &item.file_key,
                        field.ty.span.clone(),
                        "E0304",
                        "Configured process values cannot be stored in physical artifacts",
                    );
                    continue;
                }
                let mut ty = self.registry.resolve_type(&field.ty, &artifact_names, &[]);
                if ty == Type::Unknown {
                    self.push_error(
                        &item.file_key,
                        field.ty.span.clone(),
                        "E0302",
                        self.unknown_type_message("unknown field type", &field.ty),
                    );
                    // Reported: switch to the permissive type so every use of
                    // this field does not repeat the same complaint.
                    ty = Type::Error;
                }
                if matches!(ty, Type::Resource { .. }) {
                    self.push_error(
                        &item.file_key,
                        field.ty.span.clone(),
                        "E0304",
                        "capacity resources are process uses ports and cannot be stored in artifacts",
                    );
                }
                if field.derived.is_some() {
                    derived_fields.insert(field.name.clone());
                }
                fields.insert(field.name.clone(), ty);
            }
            self.model.artifacts.insert(
                artifact.name.clone(),
                ArtifactModel {
                    path: item.semantic_path.clone(),
                    fields,
                    derived_fields,
                },
            );
        }
        self.check_derived_fields();
    }

    fn check_derived_fields(&mut self) {
        let artifacts = self.artifacts.values().cloned().collect::<Vec<_>>();
        for item in artifacts {
            let Item::Artifact(artifact) = &item.item else {
                continue;
            };
            let mut env = TypeEnv::default();
            if let Some(model) = self.model.artifacts.get(&artifact.name) {
                for (name, ty) in &model.fields {
                    env.values.insert(name.clone(), ty.clone());
                }
            }
            for field in &artifact.fields {
                let Some(expression) = &field.derived else {
                    continue;
                };
                let actual = self.infer_expr(&item.file_key, expression, &env);
                // Absent only when the field's declared type failed to
                // resolve, which `resolve_artifacts` already reported.
                let expected = self
                    .model
                    .artifacts
                    .get(&artifact.name)
                    .and_then(|artifact| artifact.fields.get(&field.name))
                    .cloned()
                    .unwrap_or(Type::Error);
                if !compatible(&actual, &expected) {
                    self.push_error(
                        &item.file_key,
                        expression.span.clone(),
                        "E0303",
                        format!(
                            "derived field {} has type {actual}, expected {expected}",
                            field.name
                        ),
                    );
                }
            }
        }
    }

    fn resolve_traits_and_functions(&mut self) {
        let traits = self.traits.values().cloned().collect::<Vec<_>>();
        for item in &traits {
            let Item::Trait(trait_decl) = &item.item else {
                continue;
            };
            if CORE_CAPABILITIES.contains(&trait_decl.name.as_str()) {
                self.push_error(
                    &item.file_key,
                    trait_decl.span.clone(),
                    "E0401",
                    format!(
                        "{} is a compiler capability and cannot be redefined",
                        trait_decl.name
                    ),
                );
            }
            self.model.traits.insert(
                trait_decl.name.clone(),
                TraitModel {
                    path: item.semantic_path.clone(),
                    generics: trait_decl.generics.clone(),
                    functions: trait_decl
                        .functions
                        .iter()
                        .map(|function| function.name.clone())
                        .collect(),
                },
            );
        }
        for item in traits {
            let Item::Trait(trait_decl) = &item.item else {
                continue;
            };
            for function in &trait_decl.functions {
                self.validate_function_declaration(
                    &item,
                    function,
                    FunctionContext::Trait,
                    &trait_decl.generics,
                );
            }
        }

        let functions = self.functions.values().cloned().collect::<Vec<_>>();
        for item in functions {
            let Item::Function(function) = &item.item else {
                continue;
            };
            self.resolve_function(&item, function);
        }
        let function_bodies = self.functions.values().cloned().collect::<Vec<_>>();
        for item in function_bodies {
            let Item::Function(function) = &item.item else {
                continue;
            };
            if let Some(model) = self.model.functions.get(&function.name).cloned() {
                self.validate_pure_function_body(
                    &item,
                    function,
                    &model.parameters,
                    &model.outputs,
                );
            }
        }
        self.check_impl_coherence();
        self.check_function_recursion();
    }

    fn resolve_function(&mut self, item: &ItemRef, function: &Function) {
        self.validate_function_declaration(item, function, FunctionContext::TopLevel, &[]);
        let artifact_names = self.artifact_name_map();
        let parameters = function
            .parameters
            .iter()
            .map(|parameter| {
                (
                    parameter.name.clone(),
                    self.registry
                        .resolve_type(&parameter.ty, &artifact_names, &function.generics),
                )
            })
            .collect::<IndexMap<_, _>>();
        let outputs = function
            .outputs
            .iter()
            .map(|parameter| {
                (
                    parameter.name.clone(),
                    self.registry
                        .resolve_type(&parameter.ty, &artifact_names, &function.generics),
                )
            })
            .collect::<IndexMap<_, _>>();
        self.model.functions.insert(
            function.name.clone(),
            FunctionModel {
                path: item.semantic_path.clone(),
                generics: function.generics.clone(),
                parameters,
                outputs,
                bounds: function
                    .bounds
                    .iter()
                    .map(|bound| format!("{}: {}", bound.parameter, bound.capability))
                    .collect(),
            },
        );
    }

    fn validate_function_declaration(
        &mut self,
        item: &ItemRef,
        function: &Function,
        context: FunctionContext,
        outer_generics: &[String],
    ) {
        let mut declared_generics = outer_generics.iter().cloned().collect::<BTreeSet<_>>();
        for generic in &function.generics {
            if !declared_generics.insert(generic.clone()) {
                self.push_error(
                    &item.file_key,
                    function.span.clone(),
                    "E0427",
                    format!("duplicate generic parameter {generic}"),
                );
            }
        }
        for (kind, parameters) in [
            ("parameter", &function.parameters),
            ("result", &function.outputs),
        ] {
            let mut names = HashSet::new();
            for parameter in parameters {
                if !names.insert(parameter.name.clone()) {
                    self.push_error(
                        &item.file_key,
                        parameter.span.clone(),
                        "E0428",
                        format!("duplicate pure function {kind} {}", parameter.name),
                    );
                }
            }
        }
        if matches!(context, FunctionContext::Trait) && function.body.is_some() {
            self.push_error(
                &item.file_key,
                function.span.clone(),
                "E0429",
                "trait functions are signatures and cannot contain bodies",
            );
        }
        if matches!(context, FunctionContext::Impl)
            && (!function.generics.is_empty() || !function.bounds.is_empty())
        {
            self.push_error(
                &item.file_key,
                function.span.clone(),
                "E0430",
                "trait implementation functions cannot declare generics or bounds",
            );
        }
        let mut seen_bounds = HashSet::new();
        for bound in &function.bounds {
            if !declared_generics.contains(&bound.parameter) {
                self.push_error(
                    &item.file_key,
                    bound.span.clone(),
                    "E0431",
                    format!("bound references undeclared generic {}", bound.parameter),
                );
            }
            let capability = &bound.capability.name;
            if !(CORE_CAPABILITIES.contains(&capability.as_str())
                || self.traits.contains_key(capability)
                    && self.is_visible(&item.file_key, capability))
            {
                self.push_error(
                    &item.file_key,
                    bound.span.clone(),
                    "E0432",
                    format!("unknown generic capability {capability}"),
                );
            }
            if !seen_bounds.insert((bound.parameter.clone(), capability.clone())) {
                self.push_error(
                    &item.file_key,
                    bound.span.clone(),
                    "E0433",
                    format!("duplicate bound {}: {capability}", bound.parameter),
                );
            }
        }
        let type_parameters = declared_generics.into_iter().collect::<Vec<_>>();
        let artifact_names = self.artifact_name_map();
        for parameter in function.parameters.iter().chain(&function.outputs) {
            if parameter.ty.name == "Configured" {
                self.push_error(
                    &item.file_key,
                    parameter.ty.span.clone(),
                    "E0434",
                    "Configured process values are only valid as stable process configuration fields",
                );
                continue;
            }
            let mut ty =
                self.registry
                    .resolve_type(&parameter.ty, &artifact_names, &type_parameters);
            if ty == Type::Unknown {
                self.push_error(
                    &item.file_key,
                    parameter.ty.span.clone(),
                    "E0434",
                    self.unknown_type_message("unknown pure function type", &parameter.ty),
                );
                ty = Type::Error;
            }
            if matches!(ty, Type::Resource { .. }) {
                self.push_error(
                    &item.file_key,
                    parameter.ty.span.clone(),
                    "E0434",
                    "capacity resources are physical uses ports, not pure-function values",
                );
            }
            if type_contains_tuple(&ty) && !type_is_provably_copyable(&ty, &function.bounds) {
                self.push_error(&item.file_key, parameter.ty.span.clone(), "E0860", "named tuple fields require copyable types; generic fields need an explicit Copyable bound");
            }
        }
        for output in &function.outputs {
            let ty = self
                .registry
                .resolve_type(&output.ty, &artifact_names, &type_parameters);
            if !type_is_provably_copyable(&ty, &function.bounds) {
                self.push_error(
                    &item.file_key,
                    output.span.clone(),
                    "E0402",
                    format!(
                        "pure function output {} must be provably copyable, found {ty}",
                        output.name
                    ),
                );
            }
        }
    }

    fn validate_pure_function_body(
        &mut self,
        item: &ItemRef,
        function: &Function,
        parameters: &IndexMap<String, Type>,
        outputs: &IndexMap<String, Type>,
    ) {
        let Some(body) = &function.body else {
            self.push_error(
                &item.file_key,
                function.span.clone(),
                "E0406",
                format!("pure function {} requires a body", function.name),
            );
            return;
        };
        let mut effectful_calls = Vec::new();
        visit_block_exprs(body, &mut |expression| {
            if let ExprKind::Call { callee, .. } = &expression.kind
                && let Some(name) = callee.as_name()
                && ((self.artifacts.contains_key(name) && self.is_visible(&item.file_key, name))
                    || (self.processes.contains_key(name) && self.is_visible(&item.file_key, name)))
            {
                effectful_calls.push(expression.span.clone());
            }
        });
        for span in effectful_calls {
            self.push_error(
                &item.file_key,
                span,
                "E0426",
                "pure functions cannot construct artifacts or configure processes",
            );
        }
        let mut environment = TypeEnv::default();
        environment.values.extend(parameters.clone());
        for bound in &function.bounds {
            environment
                .capabilities
                .entry(bound.parameter.clone())
                .or_default()
                .insert(bound.capability.name.clone());
        }
        let assigned = self.validate_pure_block(
            item,
            body,
            outputs,
            &mut environment,
            PureAssignments::default(),
        );
        for (name, ty) in outputs {
            if !assigned.definite.contains(name) {
                self.push_error(
                    &item.file_key,
                    function.span.clone(),
                    "E0407",
                    format!("pure function output {name}: {ty} is not assigned on every path"),
                );
            }
        }
        if let Some(expressions) = symbolic_pure_function_outputs(function) {
            self.function_output_expressions
                .insert(function.name.clone(), expressions);
        }
    }

    /// Only ordinary mutable locals can be redeclared. Parameters, constants,
    /// ownership-bearing values and graph identities retain their existing rules.
    fn check_local_declaration(
        &mut self,
        file_key: &str,
        span: &Span,
        name: &str,
        mutable: bool,
        environment: &TypeEnv,
    ) -> bool {
        let Some(previous) = environment.values.get(name) else {
            return true;
        };
        if mutable
            && environment.declarations.contains_key(name)
            && environment.mutable.contains(name)
            && !previous.is_owned()
            && !matches!(previous, Type::ConfiguredProcess(_) | Type::Run(_))
        {
            self.push_warning(
                file_key,
                span.clone(),
                "W0601",
                format!(
                    "let binding {name} is redeclared; the new binding replaces the previous one"
                ),
            );
            true
        } else {
            self.push_error(file_key, span.clone(), "E0634",
                format!("cannot redeclare binding {name}; only mutable local let bindings may be redeclared"));
            false
        }
    }

    fn validate_pure_block(
        &mut self,
        item: &ItemRef,
        block: &Block,
        outputs: &IndexMap<String, Type>,
        environment: &mut TypeEnv,
        mut assigned: PureAssignments,
    ) -> PureAssignments {
        for statement in &block.statements {
            match statement {
                Stmt::Let {
                    mutable,
                    name,
                    ty,
                    value,
                    span,
                } => {
                    if !self.check_local_declaration(
                        &item.file_key,
                        span,
                        name,
                        *mutable,
                        environment,
                    ) {
                        continue;
                    }
                    let inferred = self.infer_expr(&item.file_key, value, environment);
                    let resolved = ty
                        .as_ref()
                        .map(|ty| {
                            self.registry.resolve_type(
                                ty,
                                &self.artifact_name_map(),
                                &environment.capabilities.keys().cloned().collect::<Vec<_>>(),
                            )
                        })
                        .unwrap_or_else(|| inferred.clone());
                    if resolved.is_owned() || inferred.is_owned() {
                        self.push_error(
                            &item.file_key,
                            span.clone(),
                            "E0408",
                            "pure functions cannot bind or move owned artifact values",
                        );
                    }
                    if matches!(resolved, Type::Resource { .. })
                        || matches!(inferred, Type::Resource { .. })
                    {
                        self.push_error(
                            &item.file_key,
                            span.clone(),
                            "E0408",
                            "pure functions cannot construct or bind capacity resources",
                        );
                    }
                    if !compatible(&resolved, &inferred) {
                        self.push_error(
                            &item.file_key,
                            span.clone(),
                            "E0409",
                            format!("binding {name} expects {resolved}, found {inferred}"),
                        );
                    }
                    environment.values.insert(name.clone(), resolved);
                    environment.declarations.insert(name.clone(), span.end);
                    if *mutable {
                        environment.mutable.insert(name.clone());
                    }
                }
                Stmt::Requires { value, span } => {
                    let ty = self.infer_expr(&item.file_key, value, environment);
                    if ty != Type::Bool && ty != Type::Error {
                        self.push_error(
                            &item.file_key,
                            span.clone(),
                            "E0410",
                            format!("pure function requirement expects Bool, found {ty}"),
                        );
                    }
                    if self.evaluate_predicate(&normalize_expr(value, &environment.expressions))
                        == Some(false)
                    {
                        self.push_error(
                            &item.file_key,
                            span.clone(),
                            "E0436",
                            "pure function requirement is statically false",
                        );
                    }
                }
                Stmt::For {
                    binding,
                    iterable,
                    body,
                    span,
                } => {
                    let Type::Array { element, .. } =
                        self.infer_expr(&item.file_key, iterable, environment)
                    else {
                        self.push_error(
                            &item.file_key,
                            span.clone(),
                            "E0411",
                            "pure function loops require a finite array",
                        );
                        continue;
                    };
                    let mut nested = environment.clone();
                    nested.values.insert(binding.clone(), *element);
                    let loop_assigned = self.validate_pure_block(
                        item,
                        body,
                        outputs,
                        &mut nested,
                        assigned.clone(),
                    );
                    assigned.maybe.extend(loop_assigned.maybe);
                }
                Stmt::Repeat { count, body, span } => {
                    let count_type = self.infer_expr(&item.file_key, count, environment);
                    if count_type != Type::Integer && count_type != Type::Error {
                        self.push_error(
                            &item.file_key,
                            span.clone(),
                            "E0437",
                            format!("repeat count expects Integer, found {count_type}"),
                        );
                    }
                    self.push_error(
                        &item.file_key,
                        span.clone(),
                        "E0438",
                        "repeat blocks schedule process runs and are not allowed in pure functions",
                    );
                    let _ = body;
                }
                Stmt::If {
                    condition,
                    then_branch,
                    else_branch,
                    span,
                } => {
                    let ty = self.infer_expr(&item.file_key, condition, environment);
                    if ty != Type::Bool && ty != Type::Error {
                        self.push_error(
                            &item.file_key,
                            span.clone(),
                            "E0412",
                            format!("pure function if condition expects Bool, found {ty}"),
                        );
                    }
                    let mut then_environment = environment.clone();
                    let then_assigned = self.validate_pure_block(
                        item,
                        then_branch,
                        outputs,
                        &mut then_environment,
                        assigned.clone(),
                    );
                    let else_assigned = if let Some(else_branch) = else_branch {
                        let mut else_environment = environment.clone();
                        self.validate_pure_block(
                            item,
                            else_branch,
                            outputs,
                            &mut else_environment,
                            assigned.clone(),
                        )
                    } else {
                        assigned.clone()
                    };
                    assigned.definite = then_assigned
                        .definite
                        .intersection(&else_assigned.definite)
                        .cloned()
                        .collect();
                    assigned.maybe = then_assigned
                        .maybe
                        .union(&else_assigned.maybe)
                        .cloned()
                        .collect();
                }
                Stmt::Assign {
                    target,
                    value,
                    span,
                } => {
                    let Some(name) = target.as_name() else {
                        self.push_error(
                            &item.file_key,
                            span.clone(),
                            "E0413",
                            "pure functions cannot mutate artifact fields",
                        );
                        continue;
                    };
                    let actual = self.infer_expr(&item.file_key, value, environment);
                    if let Some(expected) = outputs.get(name) {
                        if assigned.maybe.contains(name) {
                            self.push_error(
                                &item.file_key,
                                span.clone(),
                                "E0414",
                                format!("pure function output {name} is assigned more than once"),
                            );
                        }
                        if !compatible(expected, &actual) {
                            self.push_error(
                                &item.file_key,
                                span.clone(),
                                "E0415",
                                format!(
                                    "pure function output {name} expects {expected}, found {actual}"
                                ),
                            );
                        }
                        assigned.definite.insert(name.to_owned());
                        assigned.maybe.insert(name.to_owned());
                        environment.values.insert(name.to_owned(), expected.clone());
                    } else if environment.mutable.contains(name) {
                        // A name in `mutable` was bound with a type, so the
                        // lookup only misses when that binding already failed.
                        let expected = environment.values.get(name).cloned().unwrap_or(Type::Error);
                        if !compatible(&expected, &actual) {
                            self.push_error(
                                &item.file_key,
                                span.clone(),
                                "E0416",
                                format!("assignment to {name} expects {expected}, found {actual}"),
                            );
                        }
                    } else if environment.values.contains_key(name) {
                        self.push_error(
                            &item.file_key,
                            span.clone(),
                            "E0417",
                            format!(
                                "cannot assign immutable binding {name}; use let for mutable locals"
                            ),
                        );
                    } else {
                        self.push_error(
                            &item.file_key,
                            span.clone(),
                            "E0437",
                            format!("new local binding {name} requires `let {name} = ...`"),
                        );
                    }
                }
                Stmt::Expr { value, span } => {
                    if let ExprKind::Call { callee, .. } = &value.kind
                        && let ExprKind::Member {
                            value: receiver,
                            member,
                        } = &callee.kind
                        && member == "append"
                        && !receiver
                            .as_name()
                            .is_some_and(|name| environment.mutable.contains(name))
                    {
                        self.push_error(
                            &item.file_key,
                            span.clone(),
                            "E0418",
                            "append in a pure function requires a mutable local array",
                        );
                    }
                    if !expression_can_have_effect(value) {
                        self.push_error(
                            &item.file_key,
                            span.clone(),
                            "E0439",
                            "this expression statement has no effect; its value is discarded",
                        );
                    }
                    let inferred = self.infer_expr(&item.file_key, value, environment);
                    if matches!(inferred, Type::Resource { .. }) {
                        self.push_error(
                            &item.file_key,
                            span.clone(),
                            "E0408",
                            "pure functions cannot construct or use capacity resources",
                        );
                    }
                }
                Stmt::Yield { span, .. } | Stmt::Wire { span, .. } | Stmt::Before { span, .. } => {
                    self.push_error(
                        &item.file_key,
                        span.clone(),
                        "E0419",
                        "pure functions cannot yield, wire processes, or add scheduling edges",
                    )
                }
            }
        }
        assigned
    }

    fn check_impl_coherence(&mut self) {
        let impls = self
            .items
            .iter()
            .filter_map(|item| match &item.item {
                Item::Impl(value) => Some((item.clone(), value.clone())),
                _ => None,
            })
            .collect::<Vec<_>>();
        let mut seen = HashMap::new();
        for (item, value) in impls {
            let trait_item = self.traits.get(&value.trait_name).cloned();
            if trait_item.is_none() {
                self.push_error(
                    &item.file_key,
                    value.span.clone(),
                    "E0403",
                    format!("unknown trait {}", value.trait_name),
                );
            } else if let Some(trait_item) = trait_item {
                self.validate_trait_impl(&item, &value, &trait_item);
            }
            let artifact_names = self.artifact_name_map();
            let key = format!(
                "{}[{}]",
                value.trait_name,
                value
                    .arguments
                    .iter()
                    .map(|argument| coherence_type_key(&self.registry.resolve_type(
                        argument,
                        &artifact_names,
                        &[]
                    )))
                    .collect::<Vec<_>>()
                    .join(",")
            );
            if let Some(previous) = seen.insert(key.clone(), item.semantic_path.clone()) {
                self.push_error(
                    &item.file_key,
                    value.span,
                    "E0404",
                    format!("overlapping implementation {key}; first declared at {previous}"),
                );
            }
        }
    }

    fn validate_trait_impl(&mut self, item: &ItemRef, value: &ImplDecl, trait_item: &ItemRef) {
        let Item::Trait(trait_decl) = &trait_item.item else {
            return;
        };
        if value.arguments.len() != trait_decl.generics.len() {
            self.push_error(
                &item.file_key,
                value.span.clone(),
                "E0420",
                format!(
                    "trait {} expects {} type argument(s), found {}",
                    trait_decl.name,
                    trait_decl.generics.len(),
                    value.arguments.len()
                ),
            );
        }
        let artifact_names = self.artifact_name_map();
        let mut substitutions = IndexMap::new();
        for (name, argument) in trait_decl.generics.iter().zip(&value.arguments) {
            let mut ty = self.registry.resolve_type(argument, &artifact_names, &[]);
            if ty == Type::Unknown {
                self.push_error(
                    &item.file_key,
                    argument.span.clone(),
                    "E0435",
                    format!("unknown trait implementation type {argument}"),
                );
                ty = Type::Error;
            }
            substitutions.insert(name.clone(), ty);
        }
        let mut implementation_functions = IndexMap::new();
        for function in &value.functions {
            self.validate_function_declaration(item, function, FunctionContext::Impl, &[]);
            if implementation_functions
                .insert(function.name.clone(), function)
                .is_some()
            {
                self.push_error(
                    &item.file_key,
                    function.span.clone(),
                    "E0421",
                    format!("duplicate implementation function {}", function.name),
                );
            }
        }
        for signature in &trait_decl.functions {
            let Some(function) = implementation_functions.shift_remove(&signature.name) else {
                self.push_error(
                    &item.file_key,
                    value.span.clone(),
                    "E0422",
                    format!("implementation is missing function {}", signature.name),
                );
                continue;
            };
            if !function.generics.is_empty() {
                self.push_error(
                    &item.file_key,
                    function.span.clone(),
                    "E0423",
                    "trait implementation functions cannot introduce generic parameters",
                );
            }
            let actual_parameter_sequence = function
                .parameters
                .iter()
                .map(|parameter| {
                    (
                        parameter.name.clone(),
                        self.registry
                            .resolve_type(&parameter.ty, &artifact_names, &[]),
                    )
                })
                .collect::<Vec<_>>();
            let actual_output_sequence = function
                .outputs
                .iter()
                .map(|parameter| {
                    (
                        parameter.name.clone(),
                        self.registry
                            .resolve_type(&parameter.ty, &artifact_names, &[]),
                    )
                })
                .collect::<Vec<_>>();
            let expected_parameter_sequence = signature
                .parameters
                .iter()
                .map(|parameter| {
                    let generic = self.registry.resolve_type(
                        &parameter.ty,
                        &artifact_names,
                        &trait_decl.generics,
                    );
                    (
                        parameter.name.clone(),
                        substitute_generic_type(&generic, &substitutions),
                    )
                })
                .collect::<Vec<_>>();
            let expected_output_sequence = signature
                .outputs
                .iter()
                .map(|parameter| {
                    let generic = self.registry.resolve_type(
                        &parameter.ty,
                        &artifact_names,
                        &trait_decl.generics,
                    );
                    (
                        parameter.name.clone(),
                        substitute_generic_type(&generic, &substitutions),
                    )
                })
                .collect::<Vec<_>>();
            if actual_parameter_sequence != expected_parameter_sequence
                || actual_output_sequence != expected_output_sequence
            {
                self.push_error(
                    &item.file_key,
                    function.span.clone(),
                    "E0424",
                    format!(
                        "function {} does not match its trait signature",
                        function.name
                    ),
                );
            }
            let actual_parameters = actual_parameter_sequence.into_iter().collect();
            let actual_outputs = actual_output_sequence.into_iter().collect();
            self.validate_pure_function_body(item, function, &actual_parameters, &actual_outputs);
        }
        for (name, function) in implementation_functions {
            self.push_error(
                &item.file_key,
                function.span.clone(),
                "E0425",
                format!(
                    "function {name} is not declared by trait {}",
                    trait_decl.name
                ),
            );
        }
    }

    fn check_function_recursion(&mut self) {
        let mut calls = HashMap::<String, BTreeSet<String>>::new();
        for (name, item) in &self.functions {
            let Item::Function(function) = &item.item else {
                continue;
            };
            let mut found = BTreeSet::new();
            if let Some(body) = &function.body {
                visit_block_exprs(body, &mut |expression| {
                    if let ExprKind::Call { callee, .. } = &expression.kind
                        && let Some(name) = callee.as_name()
                        && self.functions.contains_key(name)
                    {
                        found.insert(name.to_owned());
                    }
                });
            }
            calls.insert(name.clone(), found);
        }
        for name in calls.keys() {
            if reaches(name, name, &calls, &mut HashSet::new()) {
                let item = self.functions[name].clone();
                self.push_error(
                    &item.file_key,
                    item.item.span().clone(),
                    "E0405",
                    format!("recursive pure function call graph includes {name}"),
                );
            }
        }
    }

    fn resolve_process_interfaces(&mut self) {
        let names = self.processes.keys().cloned().collect::<Vec<_>>();
        for name in names {
            let mut stack = Vec::new();
            self.flatten_process(&name, &mut stack);
        }
        let process_interfaces = self.resolved_members.clone();
        let process_monitors = self.resolved_monitors.clone();
        for (name, members) in process_interfaces {
            let item = self.processes[&name].clone();
            let Item::Process(process) = &item.item else {
                continue;
            };
            let artifact_names = self.artifact_name_map();
            let mut resource_parameter_spans = Vec::new();
            let parameters = process
                .parameters
                .iter()
                .map(|parameter| {
                    let ty = self
                        .registry
                        .resolve_type(&parameter.ty, &artifact_names, &[]);
                    if matches!(ty, Type::Resource { .. }) {
                        resource_parameter_spans.push(parameter.ty.span.clone());
                    }
                    (parameter.name.clone(), ty)
                })
                .collect();
            self.model.processes.insert(
                name.clone(),
                ProcessModel {
                    path: item.semantic_path.clone(),
                    parents: process.parents.clone(),
                    members: members
                        .iter()
                        .map(|(name, member)| {
                            (
                                name.clone(),
                                MemberModel {
                                    role: member.role,
                                    exposed: member.exposed,
                                    ty: member.ty.clone(),
                                    declared_by: member.declared_by.clone(),
                                    default: member.default.as_ref().map(Expr::display),
                                    aggregate: member.aggregate,
                                },
                            )
                        })
                        .collect(),
                    monitors: process_monitors
                        .get(&name)
                        .map(|monitors| monitors.keys().cloned().collect())
                        .unwrap_or_default(),
                    monitor_display_units: process_monitors
                        .get(&name)
                        .map(|monitors| {
                            monitors
                                .iter()
                                .filter_map(|(metric, unit)| {
                                    unit.display_unit.clone().map(|unit| (metric.clone(), unit))
                                })
                                .collect()
                        })
                        .unwrap_or_default(),
                    approximate_monitors: process_monitors
                        .get(&name)
                        .map(|monitors| {
                            monitors
                                .iter()
                                .filter(|(_, presentation)| presentation.approximate)
                                .map(|(metric, _)| metric.clone())
                                .collect()
                        })
                        .unwrap_or_default(),
                    parameters,
                    default_config: IndexMap::new(),
                    default_config_sources: IndexMap::new(),
                    default_configured_arguments: IndexMap::new(),
                    bindings: IndexMap::new(),
                    binding_declarations: IndexMap::new(),
                    bodyless: process.body.is_none(),
                    composite: false,
                    runs: IndexMap::new(),
                    wires: Vec::new(),
                    ordering: Vec::new(),
                    aggregate_contributions: IndexMap::new(),
                },
            );
            for span in resource_parameter_spans {
                self.push_error(
                    &item.file_key,
                    span,
                    "E0518",
                    "capacity resources are physical uses ports, not invocation values",
                );
            }
        }
        self.validate_process_defaults();
        self.resolve_config_overrides();
        self.publish_default_configurations();
    }

    fn resolve_config_overrides(&mut self) {
        let inputs = self.override_inputs.clone();
        for input in inputs {
            self.active_overrides
                .insert(input.path.clone(), input.value.clone());

            let Some((process_selector, field)) = input.path.rsplit_once('.') else {
                self.push_override_error(
                    "E0520",
                    format!(
                        "{} is not a process-qualified configuration path; use Process.field",
                        input.path
                    ),
                );
                continue;
            };
            if process_selector.is_empty() || field.is_empty() {
                self.push_override_error(
                    "E0520",
                    format!(
                        "{} is not a process-qualified configuration path; use Process.field",
                        input.path
                    ),
                );
                continue;
            }

            let process_name = self.processes.iter().find_map(|(name, item)| {
                (name == process_selector || item.semantic_path == process_selector)
                    .then(|| name.clone())
            });
            let Some(process_name) = process_name else {
                self.push_override_error(
                    "E0521",
                    format!("{} names an unknown process", input.path),
                );
                continue;
            };
            let Some(process_item) = self.processes.get(&process_name).cloned() else {
                continue;
            };
            let member = self
                .resolved_members
                .get(&process_name)
                .and_then(|members| members.get(field))
                .cloned();
            let Some(member) = member else {
                self.push_error(
                    &process_item.file_key,
                    process_item.item.span().clone(),
                    "E0522",
                    format!("{} names an unknown configuration field", input.path),
                );
                continue;
            };

            let declaring_item = self
                .processes
                .values()
                .find(|item| item.semantic_path == member.declared_by)
                .cloned()
                .unwrap_or_else(|| process_item.clone());
            if member.role != MemberRole::Config {
                self.push_error(
                    &declaring_item.file_key,
                    member.span.clone(),
                    "E0527",
                    format!(
                        "{} is a {} member; only exposed configuration can be overridden",
                        input.path,
                        match member.role {
                            MemberRole::Config => "config",
                            MemberRole::Gets => "gets",
                            MemberRole::Uses => "uses",
                            MemberRole::Export => "export",
                        }
                    ),
                );
                continue;
            }
            if !member.exposed {
                let source_span = self.span(&declaring_item.file_key, member.span.clone());
                self.push_error(
                    &declaring_item.file_key,
                    member.span.clone(),
                    "E0523",
                    format!(
                        "{} was not exposed; try \"expose config {}: {}\" in {}:{}",
                        input.path, field, member.ty, source_span.path, source_span.line
                    ),
                );
                continue;
            }

            let canonical_path = format!("{process_name}.{field}");
            if self
                .resolved_overrides
                .get(&process_name)
                .is_some_and(|overrides| overrides.contains_key(field))
            {
                self.push_error(
                    &declaring_item.file_key,
                    member.span.clone(),
                    "E0524",
                    format!("duplicate override for {canonical_path}"),
                );
                continue;
            }

            let expression = match crate::parser::parse_expression(&input.value) {
                Ok(expression) => expression,
                Err(errors) => {
                    let message = errors
                        .first()
                        .map_or("invalid expression", |error| error.message.as_str());
                    self.push_error(
                        &declaring_item.file_key,
                        member.span.clone(),
                        "E0525",
                        format!(
                            "invalid value for {canonical_path}: {message} in `{}`",
                            input.value
                        ),
                    );
                    continue;
                }
            };

            let mut environment = TypeEnv::default();
            if let Some(members) = self.resolved_members.get(&process_name) {
                for (name, candidate) in members {
                    if candidate.role == MemberRole::Config {
                        environment
                            .values
                            .insert(name.clone(), candidate.ty.clone());
                        environment
                            .expressions
                            .insert(name.clone(), Expr::name(name, candidate.span.clone()));
                    }
                }
            }
            let actual = self.infer_expr(&process_item.file_key, &expression, &environment);
            if actual != Type::Error && !self.types_compatible(&member.ty, &actual) {
                self.push_error(
                    &declaring_item.file_key,
                    member.span.clone(),
                    "E0526",
                    format!(
                        "override {canonical_path} expects {}, found {actual}",
                        member.ty
                    ),
                );
                continue;
            }

            self.active_overrides.shift_remove(&input.path);
            self.active_overrides
                .insert(canonical_path, input.value.clone());
            self.resolved_overrides
                .entry(process_name)
                .or_default()
                .insert(field.to_owned(), expression);
        }
    }

    fn push_override_error(&mut self, code: &str, message: String) {
        let Some(entry) = self.sources.files.get(&self.sources.entry_key) else {
            return;
        };
        let file_key = entry.key.clone();
        self.push_error(&file_key, 0..0, code, message);
    }

    fn validate_process_defaults(&mut self) {
        let process_names = self.processes.keys().cloned().collect::<Vec<_>>();
        for process_name in process_names {
            let Some(item) = self.processes.get(&process_name).cloned() else {
                continue;
            };
            let Item::Process(process) = &item.item else {
                continue;
            };
            let process = process.clone();
            let members = self
                .resolved_members
                .get(&process_name)
                .cloned()
                .unwrap_or_default();
            let mut environment = TypeEnv::default();
            for (name, member) in &members {
                if member.role == MemberRole::Config {
                    environment.values.insert(name.clone(), member.ty.clone());
                    environment
                        .expressions
                        .insert(name.clone(), Expr::name(name, member.span.clone()));
                }
            }

            let mut dependencies = HashMap::<String, BTreeSet<String>>::new();
            for (name, member) in &members {
                // Inherited defaults retain the lexical scope and diagnostics
                // of the declaration that introduced them. Validate each
                // declaration once, then reuse its checked expression while
                // publishing effective defaults for descendants.
                if member.declared_by != item.semantic_path {
                    continue;
                }
                let Some(default) = member.default.as_ref() else {
                    continue;
                };
                if member.role != MemberRole::Config {
                    continue;
                }
                let names = expression_names(default);
                for used in &names {
                    if members
                        .get(used)
                        .is_some_and(|candidate| candidate.role != MemberRole::Config)
                    {
                        self.push_error(
                            &item.file_key,
                            default.span.clone(),
                            "E0515",
                            format!(
                                "configuration default {process_name}.{name} cannot depend on non-configuration member {used}"
                            ),
                        );
                    }
                    if process
                        .parameters
                        .iter()
                        .any(|parameter| parameter.name == *used)
                    {
                        self.push_error(
                            &item.file_key,
                            default.span.clone(),
                            "E0515",
                            format!(
                                "configuration default {process_name}.{name} cannot depend on invocation parameter {used}"
                            ),
                        );
                    }
                }
                dependencies.insert(
                    name.clone(),
                    names
                        .into_iter()
                        .filter(|used| {
                            members
                                .get(used)
                                .is_some_and(|candidate| candidate.role == MemberRole::Config)
                        })
                        .collect(),
                );

                let actual = self.infer_expr(&item.file_key, default, &environment);
                if actual != Type::Error && !self.types_compatible(&member.ty, &actual) {
                    self.push_error(
                        &item.file_key,
                        default.span.clone(),
                        "E0516",
                        format!(
                            "configuration default {process_name}.{name} expects {}, found {actual}",
                            member.ty
                        ),
                    );
                }
                if let Some(configured) = self.configured_expression(default, &environment) {
                    self.validate_configuration(
                        &item,
                        &configured.process,
                        default,
                        &configured.config,
                        &environment,
                    );
                }
            }
            for name in dependencies.keys() {
                if reaches(name, name, &dependencies, &mut HashSet::new()) {
                    let span = members
                        .get(name)
                        .and_then(|member| member.default.as_ref())
                        .map_or_else(|| process.span.clone(), |default| default.span.clone());
                    self.push_error(
                        &item.file_key,
                        span,
                        "E0517",
                        format!(
                            "configuration default dependency cycle includes {process_name}.{name}"
                        ),
                    );
                }
            }
        }
    }

    fn publish_default_configurations(&mut self) {
        let names = self.processes.keys().cloned().collect::<Vec<_>>();
        for name in names {
            let configured = self.effective_root_configuration(&name);
            let Some(model) = self.model.processes.get_mut(&name) else {
                continue;
            };
            model.default_config = configured
                .config
                .iter()
                .map(|(field, value)| (field.clone(), value.display()))
                .collect();
            model.default_config_sources = configured.config_sources.clone();
            model.default_configured_arguments = configured
                .configured_arguments
                .iter()
                .map(|(field, value)| (field.clone(), configured_process_model(value)))
                .collect();
        }
    }

    fn effective_root_configuration(&self, process: &str) -> ConfiguredTemplate {
        let supplied = self
            .resolved_overrides
            .get(process)
            .cloned()
            .unwrap_or_default();
        let mut configured =
            self.effective_configuration(process, &supplied, &TypeEnv::default(), &mut Vec::new());
        for field in supplied.keys() {
            configured
                .config_sources
                .insert(field.clone(), ConfigValueSource::Overridden);
        }
        configured
    }

    fn flatten_process(
        &mut self,
        name: &str,
        stack: &mut Vec<String>,
    ) -> Option<IndexMap<String, ResolvedMember>> {
        if let Some(resolved) = self.resolved_members.get(name) {
            return Some(resolved.clone());
        }
        if stack.iter().any(|current| current == name) {
            let item = self.processes.get(name)?.clone();
            self.push_error(
                &item.file_key,
                item.item.span().clone(),
                "E0501",
                format!(
                    "process inheritance cycle: {} -> {name}",
                    stack.join(" -> ")
                ),
            );
            return None;
        }
        stack.push(name.to_owned());
        let item = self.processes.get(name)?.clone();
        let Item::Process(process) = &item.item else {
            return None;
        };
        let process = process.clone();
        let mut result = IndexMap::<String, ResolvedMember>::new();
        for parent in &process.parents {
            let Some(parent_item) = self.processes.get(parent) else {
                self.push_error(
                    &item.file_key,
                    process.span.clone(),
                    "E0502",
                    format!("unknown parent process {parent}"),
                );
                continue;
            };
            let _ = parent_item;
            if let Some(parent_members) = self.flatten_process(parent, stack) {
                for (member_name, member) in parent_members {
                    if let Some(existing) = result.get_mut(&member_name) {
                        if existing.role != member.role
                            || !compatible(&existing.ty, &member.ty)
                            || existing.aggregate != member.aggregate
                            || existing.refinement_signature != member.refinement_signature
                            || existing.default.as_ref().map(Expr::display)
                                != member.default.as_ref().map(Expr::display)
                        {
                            self.push_error(
                                &item.file_key,
                                process.span.clone(),
                                "E0503",
                                format!("conflicting inherited member {member_name}"),
                            );
                        } else {
                            // Exposure is a monotone interface promise: any
                            // inherited contract that exposes a coalesced
                            // configuration field keeps it exposed.
                            existing.exposed |= member.exposed;
                        }
                    } else {
                        result.insert(member_name, member);
                    }
                }
            }
        }
        let artifact_names = self.artifact_name_map();
        for member in &process.members {
            if result.contains_key(&member.name) {
                self.push_error(
                    &item.file_key,
                    member.span.clone(),
                    "E0504",
                    format!(
                        "process {} redeclares inherited member {}",
                        process.name, member.name
                    ),
                );
                continue;
            }
            let mut ty = self.registry.resolve_type(&member.ty, &artifact_names, &[]);
            if member.ty.name == "Configured" {
                if member.role != MemberRole::Config {
                    self.push_error(
                        &item.file_key,
                        member.span.clone(),
                        "E0508",
                        "Configured process values are stable configuration fields, not physical ports",
                    );
                    continue;
                }
                if !member.ty.arrays.is_empty() {
                    self.push_error(
                        &item.file_key,
                        member.ty.span.clone(),
                        "E0508",
                        "Configured process fields cannot be arrays",
                    );
                    continue;
                }
                let contract = match member.ty.arguments.as_slice() {
                    [TypeArgument::Type(contract)]
                        if contract.dimension.is_none()
                            && contract.arguments.is_empty()
                            && contract.arrays.is_empty() =>
                    {
                        contract
                    }
                    _ => {
                        self.push_error(
                            &item.file_key,
                            member.ty.span.clone(),
                            "E0508",
                            "Configured expects exactly one non-generic process contract type",
                        );
                        continue;
                    }
                };
                if contract.name == "Configured" {
                    self.push_error(
                        &item.file_key,
                        member.ty.span.clone(),
                        "E0508",
                        "Configured expects exactly one non-generic process contract type",
                    );
                    continue;
                }
                if !self.processes.contains_key(&contract.name)
                    || !self.is_visible(&item.file_key, &contract.name)
                {
                    self.push_error(
                        &item.file_key,
                        contract.span.clone(),
                        "E0508",
                        format!("unknown configured process contract {}", contract.name),
                    );
                    continue;
                }
            }
            if ty == Type::Unknown {
                self.push_error(
                    &item.file_key,
                    member.ty.span.clone(),
                    "E0505",
                    self.unknown_type_message("unknown process member type", &member.ty),
                );
                ty = Type::Error;
            }
            if matches!(ty, Type::Resource { .. }) {
                if !matches!(member.role, MemberRole::Uses | MemberRole::Export) {
                    self.push_error(
                        &item.file_key,
                        member.span.clone(),
                        "E0518",
                        "capacity resources may only be borrowed through uses ports or provisioned through exports",
                    );
                }
                if member.ty.arrays.len() > 1
                    || member
                        .ty
                        .arrays
                        .first()
                        .is_some_and(|array| matches!(array, ArraySize::Dynamic))
                {
                    self.push_error(
                        &item.file_key,
                        member.ty.span.clone(),
                        "E0518",
                        "a resource type accepts at most one explicit capacity claim",
                    );
                }
            }
            if member.role == MemberRole::Gets && ty != Type::Error && !ty.is_owned() {
                self.push_error(
                    &item.file_key,
                    member.span.clone(),
                    "E0506",
                    format!("gets requires a linear owned type, found {ty}"),
                );
            }
            if member.role == MemberRole::Uses && ty != Type::Error && !is_borrowable(&ty) {
                self.push_error(
                    &item.file_key,
                    member.span.clone(),
                    "E0507",
                    format!("uses requires a reusable artifact resource, found {ty}"),
                );
            }
            if member.aggregate.is_some() && member.role != MemberRole::Export {
                self.push_error(
                    &item.file_key,
                    member.span.clone(),
                    "E0509",
                    "only process exports can declare an aggregate rule",
                );
            }
            if member.default.is_some() && member.role != MemberRole::Config {
                self.push_error(
                    &item.file_key,
                    member.span.clone(),
                    "E0514",
                    "only configuration members can declare defaults",
                );
            }
            if member.aggregate.is_some() && ty != Type::Error && ty.dimension().is_none() {
                self.push_error(
                    &item.file_key,
                    member.span.clone(),
                    "E0509",
                    format!("aggregate export {} must have a numeric type", member.name),
                );
            }
            // Totalling points on an affine scale is meaningless: the sum of
            // two thermometer readings depends on where the scale's zero sits.
            // `max` and `min` stay legal because they only order the readings.
            if ty.is_absolute()
                && matches!(
                    member.aggregate,
                    Some(AggregateRule::Sum | AggregateRule::Makespan)
                )
            {
                let rule = member
                    .aggregate
                    .map_or("aggregate", |rule| rule.as_str())
                    .to_owned();
                let suggestion = ty
                    .display_unit_name()
                    .and_then(|name| self.registry.units.get(name))
                    .and_then(|unit| self.registry.relative_counterpart(unit))
                    .map_or_else(
                        || "a relative unit".to_owned(),
                        |relative| relative.name.clone(),
                    );
                self.push_error(
                    &item.file_key,
                    member.span.clone(),
                    "E0509",
                    format!(
                        "aggregate {rule} over export {} has the absolute type {ty}; adding \
                         points on a scale is meaningless. Declare it in {suggestion}, or \
                         aggregate with max or min",
                        member.name
                    ),
                );
            }
            result.insert(
                member.name.clone(),
                ResolvedMember {
                    role: member.role,
                    exposed: member.exposed,
                    ty,
                    declared_by: item.semantic_path.clone(),
                    default: member.default.clone(),
                    aggregate: member.aggregate,
                    span: member.span.clone(),
                    refinement_signature: self
                        .refinement_guards(&member.ty, &Expr::name("value", member.span.clone()))
                        .into_iter()
                        .filter_map(|statement| match statement {
                            Stmt::Requires { value, .. } => Some(value.display()),
                            _ => None,
                        })
                        .collect(),
                },
            );
        }
        let mut claim_environment = TypeEnv::default();
        claim_environment.values.extend(
            result
                .iter()
                .filter(|(_, member)| member.role == MemberRole::Config)
                .map(|(name, member)| (name.clone(), member.ty.clone())),
        );
        claim_environment
            .values
            .extend(process.parameters.iter().map(|parameter| {
                (
                    parameter.name.clone(),
                    self.registry
                        .resolve_type(&parameter.ty, &artifact_names, &[]),
                )
            }));
        for (member_name, member) in &result {
            let Type::Resource {
                name: resource_name,
                capacity,
                claim,
            } = &member.ty
            else {
                continue;
            };
            if !self.is_visible(&item.file_key, resource_name) {
                self.push_error(
                    &item.file_key,
                    member.span.clone(),
                    "E0518",
                    format!("resource type {resource_name} is not visible in this module"),
                );
                continue;
            }
            let Some(claim) = claim else {
                continue;
            };
            let claim = match crate::parser::parse_expression(claim) {
                Ok(claim) => claim,
                Err(_) => {
                    self.push_error(
                        &item.file_key,
                        member.span.clone(),
                        "E0518",
                        format!("invalid capacity claim on {member_name}"),
                    );
                    continue;
                }
            };
            let claim_type = self.infer_expr(&item.file_key, &claim, &claim_environment);
            if !compatible(capacity, &claim_type) {
                self.push_error(
                    &item.file_key,
                    member.span.clone(),
                    "E0519",
                    format!("resource claim {member_name} expects {capacity}, found {claim_type}"),
                );
            }
            let normalized = normalize_expr(&claim, &claim_environment.expressions);
            if numeric_constant(&normalized).is_some_and(|value| value <= BigRational::zero()) {
                self.push_error(
                    &item.file_key,
                    member.span.clone(),
                    "E0519",
                    format!("resource claim {member_name} must be positive"),
                );
            }
            if let (Some((claimed, claimed_dimension)), Some((declared, declared_dimension))) = (
                self.constant_quantity(&normalized),
                self.declared_resource_amount(resource_name),
            ) && claimed_dimension == declared_dimension
                && claimed > declared
            {
                self.push_error(
                    &item.file_key,
                    member.span.clone(),
                    "E0519",
                    format!(
                        "resource claim {member_name} exceeds {resource_name} capacity {}",
                        format_exact_rational(&declared)
                    ),
                );
            }
        }
        for monitor in &process.monitors {
            match result.get(&monitor.name) {
                Some(member) if member.role == MemberRole::Export && member.aggregate.is_some() => {
                    if monitor.approximate
                        && member.ty.dimension().is_none()
                        && member.ty != Type::Error
                    {
                        self.push_error(
                            &item.file_key,
                            monitor.span.clone(),
                            "E0512",
                            format!(
                                "monitor {} has nonnumeric type {} and cannot use approximate display",
                                monitor.name, member.ty
                            ),
                        );
                    }
                    if let Some(display_unit) = &monitor.display_unit {
                        let resolved_unit =
                            monitor.display_unit_expression.as_ref().map_or_else(
                                || {
                                    self.registry.units.get(display_unit).cloned().ok_or_else(
                                        || format!("unknown unit or dimension {display_unit}"),
                                    )
                                },
                                |expression| {
                                    self.registry.resolve_display_unit(display_unit, expression)
                                },
                            );
                        let unit = match resolved_unit {
                            Ok(unit) => {
                                // Downstream aggregation and scheduling carry
                                // the compact label, so intern this analyzed
                                // presentation unit for those shared paths.
                                self.registry
                                    .units
                                    .entry(display_unit.clone())
                                    .or_insert_with(|| unit.clone());
                                unit
                            }
                            Err(reason) => {
                                self.push_error(
                                    &item.file_key,
                                    monitor.span.clone(),
                                    "E0511",
                                    format!(
                                        "monitor display unit {display_unit} is invalid: {reason}"
                                    ),
                                );
                                continue;
                            }
                        };
                        match member.ty.dimension() {
                            None if member.ty != Type::Error => self.push_error(
                                &item.file_key,
                                monitor.span.clone(),
                                "E0512",
                                format!(
                                    "monitor {} has nonnumeric type {} and cannot be displayed as {display_unit}",
                                    monitor.name, member.ty
                                ),
                            ),
                            Some(dimension) if dimension != unit.dimension => {
                                // A dimensionless value is the case where the
                                // author probably wanted to *attach* a dimension.
                                // `as` cannot do that, so name the operation that
                                // can. For a genuine cross-dimension mismatch the
                                // same hint would be bad advice, so it is withheld.
                                let hint = if dimension.is_scalar() {
                                    format!(
                                        "; `as` only changes presentation, never a value's \
                                         dimension. To give a dimensionless value a dimension, \
                                         multiply by the unit: `... * 1 {display_unit}`"
                                    )
                                } else {
                                    String::new()
                                };
                                self.push_error(
                                    &item.file_key,
                                    monitor.span.clone(),
                                    "E0512",
                                    format!(
                                        "monitor {} has dimension {dimension}, incompatible with display unit {display_unit} ({}){hint}",
                                        monitor.name, unit.dimension
                                    ),
                                )
                            }
                            _ => {}
                        }
                    }
                }
                Some(_) => self.push_error(
                    &item.file_key,
                    monitor.span.clone(),
                    "E0510",
                    format!(
                        "monitor {} must name an export with an aggregate rule",
                        monitor.name
                    ),
                ),
                None => self.push_error(
                    &item.file_key,
                    monitor.span.clone(),
                    "E0510",
                    format!("monitor names unknown process member {}", monitor.name),
                ),
            }
        }
        for parameter in &process.parameters {
            if parameter.ty.name == "Configured" {
                self.push_error(
                    &item.file_key,
                    parameter.ty.span.clone(),
                    "E0508",
                    "Configured process values must be stable configuration, not invocation parameters",
                );
            }
        }
        let mut monitors = IndexMap::<String, MonitorPresentation>::new();
        for parent in &process.parents {
            if let Some(parent_monitors) = self.resolved_monitors.get(parent) {
                for (metric, display_unit) in parent_monitors {
                    monitors.insert(metric.clone(), display_unit.clone());
                }
            }
        }
        // A local clause overrides an inherited one, the same way a local
        // assignment replaces an inherited body fallback. Inherited monitors
        // need no revalidation: a contract can only monitor its own aggregate
        // exports, and every implementation inherits those.
        for monitor in &process.monitors {
            monitors.insert(
                monitor.name.clone(),
                MonitorPresentation {
                    display_unit: monitor.display_unit.clone(),
                    approximate: monitor.approximate,
                },
            );
        }
        stack.pop();
        self.resolved_monitors.insert(name.to_owned(), monitors);
        self.resolved_members
            .insert(name.to_owned(), result.clone());
        Some(result)
    }

    fn analyze_process_bodies(&mut self) {
        let names = self.processes.keys().cloned().collect::<Vec<_>>();
        let mut visiting = BTreeSet::new();
        let mut finished = BTreeSet::new();
        for name in names {
            self.analyze_process_body(&name, &mut visiting, &mut finished);
        }
    }

    fn analyze_process_body(
        &mut self,
        name: &str,
        visiting: &mut BTreeSet<String>,
        finished: &mut BTreeSet<String>,
    ) {
        if finished.contains(name) || !visiting.insert(name.to_owned()) {
            return;
        }
        let Some(item) = self.processes.get(name).cloned() else {
            visiting.remove(name);
            return;
        };
        let Item::Process(process) = &item.item else {
            visiting.remove(name);
            return;
        };
        let process = process.clone();
        for parent in &process.parents {
            self.analyze_process_body(parent, visiting, finished);
        }
        let Some(body) = process.body.clone() else {
            let members = self
                .resolved_members
                .get(&process.name)
                .cloned()
                .unwrap_or_default();
            let mut environment = TypeEnv::default();
            for (member_name, member) in &members {
                environment
                    .values
                    .insert(member_name.clone(), member.ty.clone());
            }
            let artifact_names = self.artifact_name_map();
            for parameter in &process.parameters {
                environment.values.insert(
                    parameter.name.clone(),
                    self.registry
                        .resolve_type(&parameter.ty, &artifact_names, &[]),
                );
            }
            let mut summary = ProcessSummary::default();
            let annotations = self.process_refinement_annotations(&process, &mut HashSet::new());
            let exports = self.process_export_names(&process, &mut HashSet::new());
            for (name, ty) in annotations {
                if exports.contains(&name) {
                    continue;
                }
                for statement in
                    self.refinement_guards(&ty, &Expr::name(&name, process.span.clone()))
                {
                    let Stmt::Requires { value, span } = statement else {
                        unreachable!()
                    };
                    summary.template_constraints.push(TemplateConstraint {
                        expression: value,
                        phase: "configuration".into(),
                        stream_binding: None,
                        span,
                    });
                }
            }
            self.seed_inherited_aggregate_fallbacks(
                &item,
                &process,
                &members,
                &mut environment,
                &mut summary,
            );
            let mut has_resource_export = false;
            for (export, member) in &members {
                if member.role == MemberRole::Export && matches!(member.ty, Type::Resource { .. }) {
                    has_resource_export = true;
                    self.push_obligation(
                        &item.file_key,
                        process.span.clone(),
                        "missing_export",
                        format!("{}.{}", process.name, export),
                        member.ty.to_string(),
                        format!("bodyless process does not provision resource export {export}"),
                        None,
                    );
                }
            }
            if !summary.direct_export_values.is_empty()
                || has_resource_export
                || !summary.template_constraints.is_empty()
            {
                if let Some(model) = self.model.processes.get_mut(&process.name) {
                    model.bindings = environment.values.clone();
                }
                self.publish_process_model(&process, &summary);
                self.summaries.insert(process.name.clone(), summary);
            }
            visiting.remove(name);
            finished.insert(name.to_owned());
            return;
        };
        let members = self
            .resolved_members
            .get(&process.name)
            .cloned()
            .unwrap_or_default();
        let mut environment = TypeEnv::default();
        for (name, member) in &members {
            environment.values.insert(name.clone(), member.ty.clone());
            environment
                .declarations
                .insert(name.clone(), body.span.start);
            environment
                .expressions
                .insert(name.clone(), Expr::name(name, member.span.clone()));
            if member.ty.is_owned() && matches!(member.role, MemberRole::Gets) {
                environment
                    .ownership
                    .insert(name.clone(), PlaceState::Available);
                environment
                    .lineage
                    .insert(name.clone(), BTreeSet::from([name.clone()]));
            } else if member.ty.is_owned() && matches!(member.role, MemberRole::Uses) {
                environment
                    .ownership
                    .insert(name.clone(), PlaceState::Borrowed);
            }
        }
        let artifact_names = self.artifact_name_map();
        for parameter in &process.parameters {
            if environment.values.contains_key(&parameter.name) {
                self.push_error(
                    &item.file_key,
                    parameter.span.clone(),
                    "E0634",
                    format!(
                        "process parameter {} shadows an existing visible name",
                        parameter.name
                    ),
                );
                continue;
            }
            environment.values.insert(
                parameter.name.clone(),
                self.registry
                    .resolve_type(&parameter.ty, &artifact_names, &[]),
            );
            environment.expressions.insert(
                parameter.name.clone(),
                Expr::name(&parameter.name, parameter.span.clone()),
            );
            environment
                .declarations
                .insert(parameter.name.clone(), body.span.start);
        }
        let mut summary = ProcessSummary::default();
        self.seed_inherited_aggregate_fallbacks(
            &item,
            &process,
            &members,
            &mut environment,
            &mut summary,
        );
        self.analyze_block(
            &item,
            &process,
            &body,
            &mut environment,
            &mut summary,
            None,
            None,
        );
        summary.effects = lower_effect_block(&body, &summary.runs, &environment.expressions);
        self.validate_process_graph(&item, &process, &members, &mut environment, &mut summary);
        self.finish_exports(&item, &process, &members, &mut summary);
        self.record_consumed_inputs(&item, &process, &members, &summary);
        if let Some(model) = self.model.processes.get_mut(&process.name) {
            model.bindings = environment.values.clone();
            model.binding_declarations = environment.declarations.clone();
        }
        self.publish_process_model(&process, &summary);
        self.publish_mutations(&item, &process, &summary);
        self.summaries.insert(process.name.clone(), summary);
        visiting.remove(name);
        finished.insert(name.to_owned());
    }

    fn seed_inherited_aggregate_fallbacks(
        &mut self,
        item: &ItemRef,
        process: &Process,
        members: &IndexMap<String, ResolvedMember>,
        environment: &mut TypeEnv,
        summary: &mut ProcessSummary,
    ) {
        for (export, member) in members {
            if member.role != MemberRole::Export || member.aggregate.is_none() {
                continue;
            }
            let mut fallback: Option<Expr> = None;
            for parent in &process.parents {
                let Some(value) = self
                    .summaries
                    .get(parent)
                    .and_then(|summary| summary.direct_export_values.get(export))
                    .cloned()
                else {
                    continue;
                };
                let parent_bindings = self
                    .model
                    .processes
                    .get(parent)
                    .map(|model| model.bindings.clone())
                    .unwrap_or_default();
                let mut invalid_dependencies = Vec::new();
                for dependency in expression_names(&value) {
                    match (
                        parent_bindings.get(&dependency),
                        environment.values.get(&dependency),
                    ) {
                        (Some(expected), Some(actual)) if compatible(expected, actual) => {}
                        (Some(expected), Some(actual)) => invalid_dependencies
                            .push(format!("{dependency} (parent {expected}, child {actual})")),
                        (Some(expected), None) => invalid_dependencies
                            .push(format!("{dependency} (missing; parent {expected})")),
                        (None, _)
                            if self.functions.contains_key(&dependency)
                                || matches!(
                                    dependency.as_str(),
                                    "sum" | "makespan" | "max" | "min" | "ceil" | "floor" | "sqrt"
                                ) => {}
                        (None, Some(_)) => invalid_dependencies.push(format!(
                            "{dependency} (not a value dependency of parent {parent})"
                        )),
                        (None, None) => {
                            invalid_dependencies.push(format!("{dependency} (missing)"))
                        }
                    }
                }
                if !invalid_dependencies.is_empty() {
                    self.push_error(
                        &item.file_key,
                        process.span.clone(),
                        "E0514",
                        format!(
                            "cannot inherit aggregate definition for {export}; non-inherited dependencies: {}",
                            invalid_dependencies.join(", ")
                        ),
                    );
                    continue;
                }
                if let Some(existing) = &fallback {
                    let equivalent = match (
                        self.constant_quantity(existing),
                        self.constant_quantity(&value),
                    ) {
                        (Some(left), Some(right)) => left == right,
                        _ => existing.display() == value.display(),
                    };
                    if !equivalent {
                        self.push_error(
                            &item.file_key,
                            process.span.clone(),
                            "E0513",
                            format!(
                                "conflicting inherited definitions for aggregate export {export}"
                            ),
                        );
                    }
                } else {
                    fallback = Some(value);
                }
            }
            let Some(fallback) = fallback else {
                continue;
            };
            summary
                .direct_export_values
                .insert(export.clone(), fallback.clone());
            summary
                .export_values
                .insert(export.clone(), fallback.clone());
            if member.ty.is_copyable() {
                environment.expressions.insert(export.clone(), fallback);
            }
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn analyze_block(
        &mut self,
        item: &ItemRef,
        process: &Process,
        block: &Block,
        environment: &mut TypeEnv,
        summary: &mut ProcessSummary,
        loop_context: Option<(String, String)>,
        path_condition: Option<String>,
    ) {
        for statement in &block.statements {
            match statement {
                Stmt::Let {
                    mutable,
                    name,
                    ty,
                    value,
                    span,
                } => {
                    if !self.check_local_declaration(
                        &item.file_key,
                        span,
                        name,
                        *mutable,
                        environment,
                    ) {
                        continue;
                    }
                    let inferred = self.infer_expr(&item.file_key, value, environment);
                    let production = self.artifact_production_expr(value, environment);
                    let resolved = ty
                        .as_ref()
                        .map(|ty| {
                            self.registry
                                .resolve_type(ty, &self.artifact_name_map(), &[])
                        })
                        .unwrap_or_else(|| inferred.clone());
                    if !compatible(&resolved, &inferred) {
                        self.push_error(
                            &item.file_key,
                            span.clone(),
                            "E0601",
                            format!("binding {name} expects {resolved}, found {inferred}"),
                        );
                    }
                    if matches!(resolved, Type::Resource { .. }) {
                        self.push_error(
                            &item.file_key,
                            span.clone(),
                            "E0518",
                            "resource pools cannot be bound to locals; provision exports and connect them with physical wiring",
                        );
                    }
                    let graph_binding =
                        matches!(&resolved, Type::ConfiguredProcess(_) | Type::Run(_));

                    if let Some(configured) = self.classify_configuration(value, environment) {
                        self.validate_configuration(
                            item,
                            &configured.process,
                            value,
                            &configured.config,
                            environment,
                        );
                        environment.configured.insert(name.clone(), configured);
                    } else if let Some(run) = self.classify_invocation(name, value, environment) {
                        self.validate_run_call(item, value, &run, environment);
                        environment.runs.insert(name.clone(), run.clone());
                        summary.runs.insert(name.clone(), run);
                    }

                    let mut run_dependencies = expression_run_dependencies(value, environment);
                    if environment.runs.contains_key(name) {
                        run_dependencies.insert(name.clone());
                    }

                    environment.values.insert(name.clone(), resolved.clone());
                    summary
                        .effect_binding_types
                        .insert(name.clone(), resolved.clone());
                    environment.declarations.insert(name.clone(), span.end);
                    if resolved.is_owned() {
                        environment
                            .lineage
                            .insert(name.clone(), self.owned_lineage(value, environment));
                        self.mark_consumed_run_exports(value, environment, summary);
                        self.consume_expr_owned(item, value, environment);
                    }
                    let binding_expression = if graph_binding {
                        // Run/configuration names are stable graph identities.
                        // Substituting their constructor call would erase the
                        // object path needed to resolve `run.export` later.
                        Expr::name(name, span.clone())
                    } else {
                        normalize_expr(value, &environment.expressions)
                    };
                    environment
                        .expressions
                        .insert(name.clone(), binding_expression);
                    if !run_dependencies.is_empty() {
                        environment
                            .run_dependencies
                            .insert(name.clone(), run_dependencies);
                    } else {
                        environment.run_dependencies.shift_remove(name);
                    }
                    if *mutable && !graph_binding {
                        environment.mutable.insert(name.clone());
                        summary.effect_mutable_bindings.insert(name.clone());
                    }
                    if resolved.is_owned() {
                        environment
                            .ownership
                            .insert(name.clone(), PlaceState::Available);
                    }
                    if let Some(production) = production {
                        environment
                            .production_exprs
                            .insert(name.clone(), production);
                    }
                }
                Stmt::Requires { value, span } => {
                    let ty = self.infer_expr(&item.file_key, value, environment);
                    if ty != Type::Bool && ty != Type::Error {
                        self.push_error(
                            &item.file_key,
                            span.clone(),
                            "E0602",
                            format!("requires expects Bool, found {ty}"),
                        );
                    }
                    let expression = normalize_expr(value, &environment.expressions);
                    if let Some((stream, cardinality)) = stream_cardinality_fact(&expression) {
                        if let Some(previous) = summary.stream_cardinalities.get(&stream)
                            && previous.display() != cardinality.display()
                        {
                            self.push_error(
                                &item.file_key,
                                span.clone(),
                                "E0648",
                                format!(
                                    "stream {stream} has conflicting cardinality facts: {} and {}",
                                    previous.display(),
                                    cardinality.display()
                                ),
                            );
                        } else {
                            summary.stream_cardinalities.insert(stream, cardinality);
                        }
                    }
                    if self.evaluate_predicate(&expression) == Some(false) {
                        self.push_error(
                            &item.file_key,
                            span.clone(),
                            "E0603",
                            "requirement is statically false",
                        );
                    }
                    summary.template_constraints.push(TemplateConstraint {
                        expression,
                        phase: if loop_context.is_some() {
                            "per_item".into()
                        } else {
                            "invocation".into()
                        },
                        stream_binding: loop_context.clone(),
                        span: span.clone(),
                    });
                }
                Stmt::For {
                    binding,
                    iterable,
                    body,
                    span,
                } => {
                    if environment.values.contains_key(binding) {
                        self.push_error(
                            &item.file_key,
                            span.clone(),
                            "E0634",
                            format!("loop binding {binding} shadows an existing visible name"),
                        );
                        continue;
                    }
                    let iterable_type = self.infer_expr(&item.file_key, iterable, environment);
                    let normalized_iterable = normalize_expr(iterable, &environment.expressions);
                    let cardinality = loop_cardinality(&normalized_iterable, &iterable_type);
                    let quantified_loop = matches!(&iterable_type, Type::Array { .. });
                    let consumes_iterable = iterable_type.is_owned();
                    let iterable_lineage = self.owned_lineage(iterable, environment);
                    let (element, is_stream) = match iterable_type {
                        Type::Array { element, .. } => (*element, false),
                        Type::Stream(element) => (*element, true),
                        // Propagating an iterable that already failed; the
                        // loop body should not repeat the complaint per use.
                        Type::Error => (Type::Error, false),
                        other => {
                            self.push_error(
                                &item.file_key,
                                span.clone(),
                                "E0604",
                                format!("for loop requires an array or stream, found {other}"),
                            );
                            (Type::Error, false)
                        }
                    };
                    summary
                        .effect_binding_types
                        .insert(binding.clone(), element.clone());
                    // A direct shortest-zip loop advances both input cursors
                    // only through the paired prefix. Keeping the stream roots
                    // available permits later statements to consume either
                    // tail. Storing the zip expression elsewhere still moves
                    // both inputs, avoiding an aliased lazy cursor.
                    let scoped_shortest_zip = matches!(
                        &normalized_iterable.kind,
                        ExprKind::Call { callee, .. }
                            if callee.path().as_deref() == Some("std.stream.zip")
                    );
                    if consumes_iterable && !scoped_shortest_zip {
                        self.mark_consumed_run_exports(iterable, environment, summary);
                        self.consume_expr_owned(item, iterable, environment);
                    }
                    let before_ownership = environment.ownership.clone();
                    let before_expressions = environment.expressions.clone();
                    let before_summary = summary.clone();
                    let provenance_start = self.report.provenance.len();
                    let obligation_start = self.report.obligations.len();
                    let diagnostic_start = self.report.diagnostics.len();
                    let mut nested = environment.clone();
                    nested.values.insert(binding.clone(), element.clone());
                    nested
                        .expressions
                        .insert(binding.clone(), Expr::name(binding, span.clone()));
                    if element.is_owned() {
                        nested
                            .ownership
                            .insert(binding.clone(), PlaceState::Available);
                        nested.lineage.insert(binding.clone(), iterable_lineage);
                    }
                    let stream_port = if is_stream {
                        iterable
                            .as_name()
                            .map(|port| (binding.clone(), port.to_owned()))
                    } else {
                        None
                    };
                    let mut loop_summary = summary.clone();
                    let constraint_start = loop_summary.template_constraints.len();
                    self.analyze_block(
                        item,
                        process,
                        body,
                        &mut nested,
                        &mut loop_summary,
                        stream_port.or_else(|| loop_context.clone()),
                        path_condition.clone(),
                    );
                    let mut false_loop_requirements = Vec::new();
                    if quantified_loop {
                        for constraint in loop_summary
                            .template_constraints
                            .iter_mut()
                            .skip(constraint_start)
                        {
                            if iterable_requirement_truth(
                                &normalized_iterable,
                                binding,
                                &constraint.expression,
                            ) == Some(false)
                            {
                                false_loop_requirements.push(constraint.span.clone());
                            }
                            constraint.expression = quantify_over(
                                &normalized_iterable,
                                binding,
                                &constraint.expression,
                                span,
                            );
                        }
                        let body_diagnostics = self.report.diagnostics.split_off(diagnostic_start);
                        self.report.diagnostics.extend(
                            body_diagnostics
                                .into_iter()
                                .filter(|diagnostic| diagnostic.code != "E0603"),
                        );
                        for requirement_span in false_loop_requirements {
                            self.push_error(
                                &item.file_key,
                                requirement_span,
                                "E0603",
                                "loop requirement is statically false for at least one iteration",
                            );
                        }
                    }

                    let new_assignments = loop_summary
                        .assigned_exports
                        .difference(&before_summary.assigned_exports)
                        .cloned()
                        .collect::<Vec<_>>();
                    let non_stream_assignments = new_assignments
                        .iter()
                        .filter(|name| {
                            !self
                                .resolved_members
                                .get(&process.name)
                                .and_then(|members| members.get(*name))
                                .is_some_and(|member| matches!(member.ty, Type::Stream(_)))
                        })
                        .cloned()
                        .collect::<Vec<_>>();
                    let state_mutations =
                        loop_summary.mutations.len() != before_summary.mutations.len();

                    match cardinality {
                        LoopCardinality::Zero => {
                            for export in non_stream_assignments {
                                self.push_error(
                                    &item.file_key,
                                    span.clone(),
                                    "E0630",
                                    format!(
                                        "loop cannot define non-stream export {export}; its cardinality is not exactly one"
                                    ),
                                );
                            }
                            if state_mutations {
                                self.push_error(
                                    &item.file_key,
                                    span.clone(),
                                    "E0633",
                                    "stored artifact mutation requires a loop with cardinality exactly one",
                                );
                            }
                            self.report.provenance.truncate(provenance_start);
                            self.report.obligations.truncate(obligation_start);
                            let body_diagnostics =
                                self.report.diagnostics.split_off(diagnostic_start);
                            self.report.diagnostics.extend(
                                body_diagnostics
                                    .into_iter()
                                    .filter(|diagnostic| diagnostic.code != "E0603"),
                            );
                        }
                        LoopCardinality::One => {
                            *summary = loop_summary;
                            for (name, state) in nested.ownership {
                                if environment.ownership.contains_key(&name) {
                                    environment.ownership.insert(name, state);
                                }
                            }
                            for name in environment.mutable.clone() {
                                if let Some(expression) = nested.expressions.get(&name) {
                                    environment
                                        .expressions
                                        .insert(name.clone(), expression.clone());
                                } else {
                                    environment.expressions.shift_remove(&name);
                                }
                                if let Some(lineage) = nested.lineage.get(&name) {
                                    environment.lineage.insert(name.clone(), lineage.clone());
                                }
                                if let Some(run_dependencies) = nested.run_dependencies.get(&name) {
                                    environment
                                        .run_dependencies
                                        .insert(name.clone(), run_dependencies.clone());
                                } else {
                                    environment.run_dependencies.shift_remove(&name);
                                }
                            }
                        }
                        LoopCardinality::ManyOrUnknown => {
                            *summary = loop_summary;
                            for export in &non_stream_assignments {
                                self.push_error(
                                    &item.file_key,
                                    span.clone(),
                                    "E0630",
                                    format!(
                                        "loop cannot define non-stream export {export}; its cardinality is not exactly one"
                                    ),
                                );
                                summary.assigned_exports.remove(export);
                            }
                            if state_mutations {
                                self.push_error(
                                    &item.file_key,
                                    span.clone(),
                                    "E0633",
                                    "stored artifact mutation requires a loop with cardinality exactly one",
                                );
                                summary.mutations.truncate(before_summary.mutations.len());
                            }
                            for (name, before) in &before_ownership {
                                if *before == PlaceState::Available
                                    && nested.ownership.get(name) == Some(&PlaceState::Moved)
                                {
                                    self.push_error(
                                        &item.file_key,
                                        span.clone(),
                                        "E0703",
                                        format!(
                                            "loop may move captured owned value {name} more than once"
                                        ),
                                    );
                                }
                            }
                            for name in environment.mutable.clone() {
                                match (before_expressions.get(&name), nested.expressions.get(&name))
                                {
                                    (Some(before), Some(after))
                                        if before.display() != after.display() =>
                                    {
                                        self.push_error(
                                            &item.file_key,
                                            span.clone(),
                                            "E0631",
                                            format!(
                                                "loop mutation of {name} requires an explicit fold or accumulator operation"
                                            ),
                                        );
                                    }
                                    (Some(_), None) => {
                                        // `append` is the v1 accumulator operation. Its expression
                                        // is intentionally no longer substitutable after the loop.
                                        environment.expressions.shift_remove(&name);
                                        if let Some(lineage) = nested.lineage.get(&name) {
                                            environment.lineage.insert(name, lineage.clone());
                                        }
                                    }
                                    _ => {}
                                }
                            }
                        }
                    }
                }
                Stmt::Repeat { count, body, span } => {
                    let count_type = self.infer_expr(&item.file_key, count, environment);
                    if count_type != Type::Integer && count_type != Type::Error {
                        self.push_error(
                            &item.file_key,
                            count.span.clone(),
                            "E0640",
                            format!("repeat count expects Integer, found {count_type}"),
                        );
                    }
                    let count = normalize_expr(count, &environment.expressions);
                    if numeric_constant(&count).is_some_and(|value| value < BigRational::zero()) {
                        self.push_error(
                            &item.file_key,
                            count.span.clone(),
                            "E0641",
                            "repeat count cannot be negative",
                        );
                    }
                    let before_ownership = environment.ownership.clone();
                    let before_expressions = environment.expressions.clone();
                    let before_mutable = environment.mutable.clone();
                    let before_summary = summary.clone();
                    let provenance_start = self.report.provenance.len();
                    let obligation_start = self.report.obligations.len();
                    let diagnostic_start = self.report.diagnostics.len();
                    let mut nested = environment.clone();
                    let mut repeated_summary = summary.clone();
                    self.analyze_block(
                        item,
                        process,
                        body,
                        &mut nested,
                        &mut repeated_summary,
                        loop_context.clone(),
                        path_condition.clone(),
                    );

                    let executes = !numeric_constant(&count).is_some_and(|value| value.is_zero());
                    if executes {
                        // One symbolic body analysis represents every iteration.
                        // Captured ownership must return to the same state so
                        // the next physical iteration can begin.
                        for (name, before) in &before_ownership {
                            let after = nested.ownership.get(name).copied().unwrap_or(*before);
                            if after != *before {
                                self.push_error(
                                    &item.file_key,
                                    span.clone(),
                                    "E0647",
                                    format!(
                                        "repeat body does not restore ownership of captured value {name}"
                                    ),
                                );
                            }
                        }
                        *summary = repeated_summary;
                        for (name, before) in &before_ownership {
                            if let Some(after) = nested.ownership.get(name) {
                                environment.ownership.insert(name.clone(), *after);
                            } else {
                                environment.ownership.insert(name.clone(), *before);
                            }
                        }
                        for name in before_mutable {
                            let Some(ty) = environment.values.get(&name) else {
                                continue;
                            };
                            if ty.is_owned() {
                                if matches!(ty, Type::Array { .. })
                                    && nested.expressions.get(&name).is_none()
                                {
                                    // `append` deliberately invalidates an
                                    // array's compile-time substitution.
                                    environment.expressions.shift_remove(&name);
                                } else {
                                    // The artifact identity is loop-carried. A
                                    // concrete iteration's run path must not
                                    // escape as the final carried value.
                                    environment
                                        .expressions
                                        .insert(name.clone(), Expr::name(&name, span.clone()));
                                }
                            } else {
                                match nested.expressions.get(&name) {
                                    Some(after)
                                        if before_expressions.get(&name).is_some_and(
                                            |before| before.display() == after.display(),
                                        ) =>
                                    {
                                        environment.expressions.insert(name.clone(), after.clone());
                                    }
                                    _ => {
                                        // An appended collection or recurrence
                                        // is valid state, but no longer a safe
                                        // compile-time substitution.
                                        environment.expressions.shift_remove(&name);
                                    }
                                }
                            }
                            if let Some(lineage) = nested.lineage.get(&name) {
                                environment.lineage.insert(name.clone(), lineage.clone());
                            }
                            if let Some(dependencies) = nested.run_dependencies.get(&name) {
                                environment
                                    .run_dependencies
                                    .insert(name.clone(), dependencies.clone());
                            }
                        }
                    } else {
                        // A statically empty repeat has no ownership or value
                        // effect. Its body remains syntax/type checked, but
                        // cannot create constraints, obligations, or graph work.
                        environment.ownership = before_ownership;
                        environment.expressions = before_expressions;
                        *summary = before_summary;
                        self.report.provenance.truncate(provenance_start);
                        self.report.obligations.truncate(obligation_start);
                        let body_diagnostics = self.report.diagnostics.split_off(diagnostic_start);
                        self.report.diagnostics.extend(
                            body_diagnostics
                                .into_iter()
                                .filter(|diagnostic| diagnostic.code != "E0603"),
                        );
                    }
                }
                Stmt::If {
                    condition,
                    then_branch,
                    else_branch,
                    span,
                } => {
                    let condition_type = self.infer_expr(&item.file_key, condition, environment);
                    if condition_type != Type::Bool && condition_type != Type::Error {
                        self.push_error(
                            &item.file_key,
                            condition.span.clone(),
                            "E0605",
                            format!("if condition expects Bool, found {condition_type}"),
                        );
                    }
                    let condition = normalize_expr(condition, &environment.expressions);
                    let before_assigned = summary.assigned_exports.clone();
                    let before_export_values = summary.export_values.clone();
                    let before_constraint_count = summary.template_constraints.len();
                    let before_yield_counts = summary
                        .yields
                        .iter()
                        .map(|(name, templates)| (name.clone(), templates.len()))
                        .collect::<HashMap<_, _>>();
                    let mut then_env = environment.clone();
                    let mut then_summary = summary.clone();
                    self.analyze_block(
                        item,
                        process,
                        then_branch,
                        &mut then_env,
                        &mut then_summary,
                        loop_context.clone(),
                        Some(combine_condition(
                            path_condition.as_deref(),
                            &condition.display(),
                            false,
                        )),
                    );
                    let mut else_env = environment.clone();
                    let mut else_summary = summary.clone();
                    if let Some(else_branch) = else_branch {
                        self.analyze_block(
                            item,
                            process,
                            else_branch,
                            &mut else_env,
                            &mut else_summary,
                            loop_context.clone(),
                            Some(combine_condition(
                                path_condition.as_deref(),
                                &condition.display(),
                                true,
                            )),
                        );
                    }
                    for name in environment.ownership.keys().cloned().collect::<Vec<_>>() {
                        let then_state = then_env.ownership.get(&name);
                        let else_state = else_env.ownership.get(&name);
                        if then_state != else_state {
                            self.push_error(
                                &item.file_key,
                                span.clone(),
                                "E0606",
                                format!("owned value {name} has path-dependent ownership after if"),
                            );
                        } else if let Some(state) = then_state {
                            environment.ownership.insert(name, *state);
                        }
                    }
                    for name in environment.mutable.iter().cloned().collect::<Vec<_>>() {
                        let then_value = then_env.expressions.get(&name);
                        let else_value = else_env.expressions.get(&name);
                        match (then_value, else_value) {
                            (Some(left), Some(right)) if left.display() == right.display() => {
                                environment.expressions.insert(name.clone(), left.clone());
                            }
                            (Some(left), Some(right)) if left.display() != right.display() => {
                                environment.expressions.insert(
                                    name.clone(),
                                    Expr {
                                        kind: ExprKind::Call {
                                            callee: Box::new(Expr::name(
                                                "if_then_else",
                                                condition.span.clone(),
                                            )),
                                            arguments: vec![
                                                CallArgument {
                                                    name: None,
                                                    value: condition.clone(),
                                                    span: condition.span.clone(),
                                                },
                                                CallArgument {
                                                    name: None,
                                                    value: left.clone(),
                                                    span: left.span.clone(),
                                                },
                                                CallArgument {
                                                    name: None,
                                                    value: right.clone(),
                                                    span: right.span.clone(),
                                                },
                                            ],
                                        },
                                        span: span.clone(),
                                    },
                                );
                            }
                            (None, None) => {
                                environment.expressions.shift_remove(&name);
                            }
                            _ => {
                                // One branch has an expression and the other is deliberately
                                // unknown (for example, an appended accumulator). Choosing the
                                // known branch would be an unsound substitution after the merge.
                                environment.expressions.shift_remove(&name);
                            }
                        }
                        let mut merged_lineage =
                            then_env.lineage.get(&name).cloned().unwrap_or_default();
                        merged_lineage
                            .extend(else_env.lineage.get(&name).cloned().unwrap_or_default());
                        if !merged_lineage.is_empty() {
                            environment.lineage.insert(name.clone(), merged_lineage);
                        }
                        let mut run_dependencies = then_env
                            .run_dependencies
                            .get(&name)
                            .cloned()
                            .unwrap_or_default();
                        run_dependencies.extend(
                            else_env
                                .run_dependencies
                                .get(&name)
                                .into_iter()
                                .flatten()
                                .cloned(),
                        );
                        if run_dependencies.is_empty() {
                            environment.run_dependencies.shift_remove(&name);
                        } else {
                            environment
                                .run_dependencies
                                .insert(name.clone(), run_dependencies);
                        }
                    }
                    let then_new = then_summary
                        .assigned_exports
                        .difference(&before_assigned)
                        .cloned()
                        .collect::<BTreeSet<_>>();
                    let else_new = else_summary
                        .assigned_exports
                        .difference(&before_assigned)
                        .cloned()
                        .collect::<BTreeSet<_>>();
                    for export in then_new.symmetric_difference(&else_new) {
                        let is_stream = self
                            .resolved_members
                            .get(&process.name)
                            .and_then(|members| members.get(export))
                            .is_some_and(|member| matches!(member.ty, Type::Stream(_)));
                        if is_stream {
                            continue;
                        }
                        // An inherited aggregate definition is a path-local fallback.
                        // A child assignment replaces it only on the branch where the
                        // assignment executes, so the other branch remains defined.
                        if before_export_values.contains_key(export) {
                            continue;
                        }
                        self.push_obligation(
                            &item.file_key,
                            span.clone(),
                            "missing_export",
                            format!("{}.{}", process.name, export),
                            self.resolved_members
                                .get(&process.name)
                                .and_then(|members| members.get(export))
                                .map(|member| member.ty.to_string())
                                .unwrap_or_else(|| "Unknown".into()),
                            format!("export {export} is undefined on one symbolic branch"),
                            Some(if then_new.contains(export) {
                                format!("!({})", condition.display())
                            } else {
                                condition.display()
                            }),
                        );
                    }
                    summary.surviving_inputs.extend(
                        then_summary
                            .surviving_inputs
                            .intersection(&else_summary.surviving_inputs)
                            .cloned(),
                    );
                    summary.template_constraints.extend(
                        then_summary
                            .template_constraints
                            .iter()
                            .skip(before_constraint_count)
                            .cloned(),
                    );
                    summary.template_constraints.extend(
                        else_summary
                            .template_constraints
                            .iter()
                            .skip(before_constraint_count)
                            .cloned(),
                    );
                    for branch in [&then_summary, &else_summary] {
                        for (name, templates) in &branch.yields {
                            let previous = before_yield_counts.get(name).copied().unwrap_or(0);
                            summary
                                .yields
                                .entry(name.clone())
                                .or_default()
                                .extend(templates.iter().skip(previous).cloned());
                        }
                        for (name, timing) in &branch.stream_first_emissions {
                            if let Some(previous) = summary.stream_first_emissions.get(name) {
                                if previous.after.display() != timing.after.display() {
                                    self.push_error(
                                        &item.file_key,
                                        span.clone(),
                                        "E0619",
                                        format!(
                                            "stream {name} has conflicting first-emission timings across branches"
                                        ),
                                    );
                                }
                            } else {
                                summary
                                    .stream_first_emissions
                                    .insert(name.clone(), timing.clone());
                            }
                        }
                    }
                    for export in then_new.union(&else_new) {
                        let (Some(left), Some(right)) = (
                            then_summary.export_values.get(export),
                            else_summary.export_values.get(export),
                        ) else {
                            continue;
                        };
                        let merged = if left.display() == right.display() {
                            left.clone()
                        } else {
                            Expr {
                                kind: ExprKind::Call {
                                    callee: Box::new(Expr::name(
                                        "if_then_else",
                                        condition.span.clone(),
                                    )),
                                    arguments: vec![
                                        CallArgument {
                                            name: None,
                                            value: condition.clone(),
                                            span: condition.span.clone(),
                                        },
                                        CallArgument {
                                            name: None,
                                            value: left.clone(),
                                            span: left.span.clone(),
                                        },
                                        CallArgument {
                                            name: None,
                                            value: right.clone(),
                                            span: right.span.clone(),
                                        },
                                    ],
                                },
                                span: span.clone(),
                            }
                        };
                        summary.export_values.insert(export.clone(), merged);
                        summary.assigned_exports.insert(export.clone());
                    }
                    for (export, value) in before_export_values {
                        summary.export_values.entry(export).or_insert(value);
                    }
                    summary.assigned_exports.extend(
                        then_new
                            .union(&else_new)
                            .filter(|export| {
                                self.resolved_members
                                    .get(&process.name)
                                    .and_then(|members| members.get(*export))
                                    .is_some_and(|member| matches!(member.ty, Type::Stream(_)))
                            })
                            .cloned(),
                    );
                }
                Stmt::Yield {
                    target,
                    value,
                    after,
                    span,
                } => {
                    let Some(port) = target.as_name() else {
                        self.push_error(
                            &item.file_key,
                            target.span.clone(),
                            "E0607",
                            "yield target must be a stream export name",
                        );
                        continue;
                    };
                    let expected = self
                        .resolved_members
                        .get(&process.name)
                        .and_then(|members| members.get(port))
                        .map(|member| member.ty.clone())
                        .unwrap_or(Type::Error);
                    let actual = self.infer_expr(&item.file_key, value, environment);
                    let item_type = match expected {
                        Type::Stream(item_type) => *item_type,
                        other => {
                            self.push_error(
                                &item.file_key,
                                target.span.clone(),
                                "E0608",
                                format!("yield target {port} is {other}, not Stream[T]"),
                            );
                            Type::Error
                        }
                    };
                    if !compatible(&item_type, &actual) {
                        self.push_error(
                            &item.file_key,
                            value.span.clone(),
                            "E0609",
                            format!("yielded {actual}, expected {item_type}"),
                        );
                    }
                    if let Some(after) = after {
                        let actual_after = self.infer_expr(&item.file_key, after, environment);
                        let expected_after = self.time_type();
                        if !compatible(&expected_after, &actual_after) {
                            self.push_error(
                                &item.file_key,
                                after.span.clone(),
                                "E0618",
                                format!("yield timing expects Time, found {actual_after}"),
                            );
                        } else {
                            let after = normalize_expr(after, &environment.expressions);
                            if let Some(previous) = summary.stream_first_emissions.get(port) {
                                if previous.after.display() != after.display() {
                                    self.push_error(
                                        &item.file_key,
                                        after.span.clone(),
                                        "E0619",
                                        format!(
                                            "stream {port} has conflicting first-emission timings"
                                        ),
                                    );
                                }
                            } else {
                                summary.stream_first_emissions.insert(
                                    port.to_owned(),
                                    StreamEmissionTemplate {
                                        after,
                                        span: span.clone(),
                                    },
                                );
                            }
                        }
                    }
                    summary.assigned_exports.insert(port.to_owned());
                    if let Some(template) = self.extract_yield_template(value, environment, span) {
                        summary
                            .yields
                            .entry(port.to_owned())
                            .or_default()
                            .push(template);
                    }
                    self.mark_surviving_inputs(process, value, environment, summary);
                    self.mark_consumed_run_exports(value, environment, summary);
                    self.consume_expr_owned(item, value, environment);
                    self.record_artifact_production(item, process, port, value, environment);
                }
                Stmt::Wire {
                    source,
                    target,
                    span,
                } => {
                    if let Some(source_name) = source.as_name()
                        && self
                            .resolved_members
                            .get(&process.name)
                            .is_none_or(|members| !members.contains_key(source_name))
                        && environment
                            .values
                            .get(source_name)
                            .is_some_and(Type::is_owned)
                    {
                        self.consume_expr_owned(item, source, environment);
                    }
                    summary
                        .wires
                        .push((source.clone(), target.clone(), span.clone()));
                }
                Stmt::Before {
                    first,
                    second,
                    span,
                } => match (first.as_name(), second.as_name()) {
                    (Some(first), Some(second)) => {
                        summary.ordering.push((first.into(), second.into()));
                    }
                    _ => self.push_error(
                        &item.file_key,
                        span.clone(),
                        "E0610",
                        "before requires two run names",
                    ),
                },
                Stmt::Assign {
                    target,
                    value,
                    span,
                } => {
                    self.analyze_assignment(
                        item,
                        process,
                        target,
                        value,
                        span,
                        environment,
                        summary,
                    );
                }
                Stmt::Expr { value, .. } => {
                    if !expression_can_have_effect(value) {
                        self.push_error(
                            &item.file_key,
                            value.span.clone(),
                            "E0439",
                            "this expression statement has no effect; its value is discarded",
                        );
                    }
                    let inferred = self.infer_expr(&item.file_key, value, environment);
                    if matches!(inferred, Type::Resource { .. }) {
                        self.push_error(
                            &item.file_key,
                            value.span.clone(),
                            "E0518",
                            "resource pools cannot be used as ordinary values; assign a constructor directly to an export or connect ports with physical wiring",
                        );
                    }
                    if let ExprKind::Call { callee, arguments } = &value.kind
                        && let ExprKind::Member {
                            value: receiver,
                            member,
                        } = &callee.kind
                        && member == "append"
                    {
                        let appended_lineage = arguments
                            .iter()
                            .flat_map(|argument| self.owned_lineage(&argument.value, environment))
                            .collect::<BTreeSet<_>>();
                        let appended_run_dependencies = arguments
                            .iter()
                            .flat_map(|argument| {
                                expression_run_dependencies(&argument.value, environment)
                            })
                            .collect::<BTreeSet<_>>();
                        if let Some(receiver) = receiver.as_name() {
                            if !environment.mutable.contains(receiver) {
                                self.push_error(
                                    &item.file_key,
                                    value.span.clone(),
                                    "E0626",
                                    format!("append requires mutable local {receiver}"),
                                );
                                continue;
                            }
                            // Append mutates the local collection. Keeping its initializer as a
                            // substitution would incorrectly reduce later sums to the empty array.
                            environment.expressions.shift_remove(receiver);
                            environment
                                .lineage
                                .entry(receiver.to_owned())
                                .or_default()
                                .extend(appended_lineage);
                            environment
                                .run_dependencies
                                .entry(receiver.to_owned())
                                .or_default()
                                .extend(appended_run_dependencies);
                        }
                        for argument in arguments {
                            self.mark_consumed_run_exports(&argument.value, environment, summary);
                            self.consume_expr_owned(item, &argument.value, environment);
                        }
                    }
                }
            }
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn analyze_assignment(
        &mut self,
        item: &ItemRef,
        process: &Process,
        target: &Expr,
        value: &Expr,
        span: &Span,
        environment: &mut TypeEnv,
        summary: &mut ProcessSummary,
    ) {
        if let Some(name) = target.as_name() {
            if let Some(member) = self
                .resolved_members
                .get(&process.name)
                .and_then(|members| members.get(name))
                .cloned()
            {
                if member.role != MemberRole::Export {
                    self.push_error(
                        &item.file_key,
                        target.span.clone(),
                        "E0611",
                        format!("cannot assign process member {name}; only exports are assignable"),
                    );
                    return;
                }
                if summary.assigned_exports.contains(name) {
                    self.push_error(
                        &item.file_key,
                        span.clone(),
                        "E0612",
                        format!("export {name} is assigned more than once on one path"),
                    );
                }
                let actual = self.infer_expr(&item.file_key, value, environment);
                if !compatible(&member.ty, &actual) {
                    self.push_error(
                        &item.file_key,
                        value.span.clone(),
                        "E0613",
                        format!("export {name} expects {}, found {actual}", member.ty),
                    );
                }
                if matches!(member.ty, Type::Resource { .. }) {
                    if let Some(constructor) = self.resource_constructor(value) {
                        if !self.resource_constructor_matches_export(
                            &member.ty,
                            &constructor,
                            environment,
                        ) {
                            self.push_error(
                                &item.file_key,
                                value.span.clone(),
                                "E0613",
                                format!(
                                    "resource constructor for {name} must provision the capacity declared by the export"
                                ),
                            );
                        }
                        summary
                            .provisioned_resources
                            .insert(name.to_owned(), value.clone());
                    } else {
                        self.push_error(
                            &item.file_key,
                            value.span.clone(),
                            "E0613",
                            format!(
                                "resource export {name} must be fulfilled by a matching resource constructor or a physical re-export wire"
                            ),
                        );
                    }
                }
                summary.assigned_exports.insert(name.to_owned());
                let normalized = normalize_expr(value, &environment.expressions);
                if member.aggregate.is_some() {
                    let dependencies = expression_names(&normalized);
                    if dependencies.iter().any(|used| used == name) {
                        self.push_error(
                            &item.file_key,
                            value.span.clone(),
                            "E0627",
                            format!(
                                "direct contribution {name} cannot depend on its own derived total"
                            ),
                        );
                    }
                    let child_runs = expression_run_dependencies(value, environment)
                        .into_iter()
                        .collect::<Vec<_>>();
                    if member.aggregate != Some(AggregateRule::Explicit) && !child_runs.is_empty() {
                        self.push_error(
                            &item.file_key,
                            value.span.clone(),
                            "E0628",
                            format!(
                                "direct contribution {name} cannot absorb child-run totals ({}) because aggregation includes them automatically",
                                child_runs.join(", ")
                            ),
                        );
                    }
                }
                summary
                    .export_values
                    .insert(name.to_owned(), normalized.clone());
                if member.ty.is_copyable() {
                    environment.expressions.insert(name.to_owned(), normalized);
                }
                self.mark_surviving_inputs(process, value, environment, summary);
                self.mark_consumed_run_exports(value, environment, summary);
                self.consume_expr_owned(item, value, environment);
                self.record_artifact_production(item, process, name, value, environment);
                return;
            }

            if environment.mutable.contains(name) {
                let actual = self.infer_expr(&item.file_key, value, environment);
                // As above: `mutable` membership implies a recorded type, so a
                // miss means the original binding was already diagnosed.
                let expected = environment.values.get(name).cloned().unwrap_or(Type::Error);
                let lineage = expected
                    .is_owned()
                    .then(|| self.owned_lineage(value, environment));
                let production = self.artifact_production_expr(value, environment);
                if !compatible(&expected, &actual) {
                    self.push_error(
                        &item.file_key,
                        value.span.clone(),
                        "E0614",
                        format!("assignment to {name} expects {expected}, found {actual}"),
                    );
                }
                if expected.is_owned() {
                    self.mark_consumed_run_exports(value, environment, summary);
                    self.consume_expr_owned(item, value, environment);
                    environment
                        .ownership
                        .insert(name.to_owned(), PlaceState::Available);
                    environment
                        .lineage
                        .insert(name.to_owned(), lineage.unwrap_or_default());
                    if let Some(production) = production {
                        environment
                            .production_exprs
                            .insert(name.to_owned(), production);
                    } else {
                        environment.production_exprs.shift_remove(name);
                    }
                }
                environment.expressions.insert(
                    name.to_owned(),
                    normalize_expr(value, &environment.expressions),
                );
                let run_dependencies = expression_run_dependencies(value, environment);
                if run_dependencies.is_empty() {
                    environment.run_dependencies.shift_remove(name);
                } else {
                    environment
                        .run_dependencies
                        .insert(name.to_owned(), run_dependencies);
                }
                return;
            }

            if environment.values.contains_key(name) {
                self.push_error(
                    &item.file_key,
                    target.span.clone(),
                    "E0625",
                    format!("cannot assign immutable binding {name}; use let for mutable locals"),
                );
                return;
            }

            // Keep parsing bare assignments so this produces one targeted semantic
            // diagnostic instead of a cascade of parser errors. New locals are
            // introduced exclusively with `let`; assignment is reserved for
            // declared exports/results and existing mutable storage.
            let inferred = self.infer_expr(&item.file_key, value, environment);
            self.push_error(
                &item.file_key,
                target.span.clone(),
                "E0635",
                format!(
                    "new local binding {name} requires `let {name} = ...`; bare assignment is reserved for declared exports and mutable locals"
                ),
            );

            // Retain a recovered binding after the diagnostic so later uses can
            // still be checked and invalid programs remain useful to `inspect`
            // and the graph renderer.
            if let Some(configured) = self.classify_configuration(value, environment) {
                self.validate_configuration(
                    item,
                    &configured.process,
                    value,
                    &configured.config,
                    environment,
                );
                environment.values.insert(
                    name.to_owned(),
                    Type::ConfiguredProcess(configured.process.clone()),
                );
                environment.declarations.insert(name.to_owned(), span.end);
                environment.configured.insert(name.to_owned(), configured);
                return;
            }
            if let Some(run) = self.classify_invocation(name, value, environment) {
                self.validate_run_call(item, value, &run, environment);
                environment
                    .values
                    .insert(name.to_owned(), Type::Run(run.process.clone()));
                environment.declarations.insert(name.to_owned(), span.end);
                environment.runs.insert(name.to_owned(), run.clone());
                summary.runs.insert(name.to_owned(), run);
                return;
            }

            let production = self.artifact_production_expr(value, environment);
            let lineage = inferred
                .is_owned()
                .then(|| self.owned_lineage(value, environment));
            if inferred.is_owned() {
                self.mark_consumed_run_exports(value, environment, summary);
                self.consume_expr_owned(item, value, environment);
            }
            environment.values.insert(name.to_owned(), inferred.clone());
            environment.declarations.insert(name.to_owned(), span.end);
            environment.expressions.insert(
                name.to_owned(),
                normalize_expr(value, &environment.expressions),
            );
            if inferred.is_owned() {
                environment
                    .ownership
                    .insert(name.to_owned(), PlaceState::Available);
                environment
                    .lineage
                    .insert(name.to_owned(), lineage.unwrap_or_default());
            }
            if let Some(production) = production {
                environment
                    .production_exprs
                    .insert(name.to_owned(), production);
            }
            return;
        }

        if let ExprKind::Member {
            value: root,
            member,
        } = &target.kind
        {
            let Some(root_name) = root.as_name() else {
                self.push_error(
                    &item.file_key,
                    target.span.clone(),
                    "E0615",
                    "field assignment root must be a named artifact",
                );
                return;
            };
            let root_type = environment
                .values
                .get(root_name)
                .cloned()
                .unwrap_or(Type::Error);
            let Type::Artifact(artifact_name) = root_type else {
                self.push_error(
                    &item.file_key,
                    target.span.clone(),
                    "E0616",
                    "field assignment requires an owned or exclusively borrowed artifact",
                );
                return;
            };
            if self
                .model
                .artifacts
                .get(&artifact_name)
                .is_some_and(|artifact| artifact.derived_fields.contains(member))
            {
                self.push_error(
                    &item.file_key,
                    target.span.clone(),
                    "E0617",
                    format!("derived field {artifact_name}.{member} is read-only"),
                );
                return;
            }
            let member_role = self
                .resolved_members
                .get(&process.name)
                .and_then(|members| members.get(root_name))
                .map(|member| member.role);
            let allowed = match member_role {
                Some(MemberRole::Gets) => {
                    environment.ownership.get(root_name) == Some(&PlaceState::Available)
                }
                Some(MemberRole::Uses) => {
                    environment.ownership.get(root_name) == Some(&PlaceState::Borrowed)
                }
                _ => environment.ownership.get(root_name) == Some(&PlaceState::Available),
            };
            if !allowed {
                self.push_error(
                    &item.file_key,
                    target.span.clone(),
                    "E0618",
                    format!("{root_name} is not owned or exclusively borrowed"),
                );
                return;
            }
            let expected = self
                .model
                .artifacts
                .get(&artifact_name)
                .and_then(|artifact| artifact.fields.get(member))
                .cloned()
                .unwrap_or(Type::Error);
            let actual = self.infer_expr(&item.file_key, value, environment);
            if !compatible(&expected, &actual) {
                self.push_error(
                    &item.file_key,
                    value.span.clone(),
                    "E0619",
                    format!("field {artifact_name}.{member} expects {expected}, found {actual}"),
                );
            }
            summary.mutations.push(MutationTemplate {
                artifact: root_name.into(),
                field: member.clone(),
                before: target.display(),
                after: normalize_expr(value, &environment.expressions).display(),
                span: span.clone(),
            });
            return;
        }

        self.push_error(
            &item.file_key,
            target.span.clone(),
            "E0620",
            "unsupported assignment target",
        );
    }

    fn classify_configuration(
        &self,
        expression: &Expr,
        environment: &TypeEnv,
    ) -> Option<ConfiguredTemplate> {
        self.configured_expression(expression, environment)
    }

    fn configured_expression(
        &self,
        expression: &Expr,
        environment: &TypeEnv,
    ) -> Option<ConfiguredTemplate> {
        self.configured_expression_inner(expression, environment, &mut Vec::new())
    }

    fn configured_expression_inner(
        &self,
        expression: &Expr,
        environment: &TypeEnv,
        stack: &mut Vec<String>,
    ) -> Option<ConfiguredTemplate> {
        if let Some(name) = expression.as_name() {
            return environment.configured.get(name).cloned().or_else(|| {
                let Type::ConfiguredProcess(process) = environment.values.get(name)? else {
                    return None;
                };
                Some(ConfiguredTemplate {
                    process: process.clone(),
                    process_source: Some(name.to_owned()),
                    config: IndexMap::new(),
                    config_sources: IndexMap::new(),
                    configured_arguments: IndexMap::new(),
                })
            });
        }
        let ExprKind::Call { callee, arguments } = &expression.kind else {
            return None;
        };
        let process = callee.as_name()?;
        if !self.processes.contains_key(process) || environment.configured.contains_key(process) {
            return None;
        }
        let supplied = process_arguments(arguments);
        Some(self.effective_configuration(process, &supplied, environment, stack))
    }

    fn effective_configuration(
        &self,
        process: &str,
        supplied: &IndexMap<String, Expr>,
        environment: &TypeEnv,
        stack: &mut Vec<String>,
    ) -> ConfiguredTemplate {
        let mut config = supplied
            .iter()
            .map(|(name, value)| {
                (
                    name.clone(),
                    normalize_expr(value, &environment.expressions),
                )
            })
            .collect::<IndexMap<_, _>>();
        let mut config_sources = supplied
            .keys()
            .map(|name| (name.clone(), ConfigValueSource::Supplied))
            .collect::<IndexMap<_, _>>();
        let members = self
            .resolved_members
            .get(process)
            .cloned()
            .unwrap_or_default();

        let entered = !stack.iter().any(|active| active == process);
        if entered {
            stack.push(process.to_owned());
            let mut resolving = BTreeSet::new();
            for (name, member) in &members {
                if member.role != MemberRole::Config || config.contains_key(name) {
                    continue;
                }
                if let Some(value) = resolve_config_default(
                    name,
                    &members,
                    supplied,
                    environment,
                    &mut config,
                    &mut resolving,
                ) {
                    config.insert(name.clone(), value);
                    config_sources.insert(name.clone(), ConfigValueSource::Defaulted);
                }
            }
        }

        let configured_arguments = if entered {
            let mut configured_environment = environment.clone();
            configured_environment.expressions.extend(config.clone());
            config
                .iter()
                .filter_map(|(name, _)| {
                    let source = supplied
                        .get(name)
                        .or_else(|| members.get(name).and_then(|member| member.default.as_ref()))?;
                    self.configured_expression_inner(source, &configured_environment, stack)
                        .map(|configured| (name.clone(), configured))
                })
                .collect()
        } else {
            IndexMap::new()
        };
        if entered {
            stack.pop();
        }
        ConfiguredTemplate {
            process: process.into(),
            process_source: None,
            config,
            config_sources,
            configured_arguments,
        }
    }

    fn classify_invocation(
        &self,
        _run_name: &str,
        expression: &Expr,
        environment: &TypeEnv,
    ) -> Option<RunTemplate> {
        let ExprKind::Call { callee, arguments } = &expression.kind else {
            return None;
        };
        if let Some(configured) = self.configured_expression(callee, environment) {
            let configured_by = callee
                .as_name()
                .map(str::to_owned)
                .unwrap_or_else(|| configured.process.clone());
            return Some(RunTemplate {
                process: configured.process.clone(),
                process_source: configured.process_source.clone(),
                configured_by,
                config: configured.config.clone(),
                config_sources: configured.config_sources.clone(),
                configured_arguments: configured.configured_arguments.clone(),
                arguments: process_arguments(arguments)
                    .into_iter()
                    .map(|(name, value)| (name, normalize_expr(&value, &environment.expressions)))
                    .collect(),
                span: expression.span.clone(),
            });
        }
        let configured_name = callee.as_name()?;
        let Type::ConfiguredProcess(contract) = environment.values.get(configured_name)? else {
            return None;
        };
        Some(RunTemplate {
            process: contract.clone(),
            process_source: Some(configured_name.into()),
            configured_by: configured_name.into(),
            config: IndexMap::new(),
            config_sources: IndexMap::new(),
            configured_arguments: IndexMap::new(),
            arguments: process_arguments(arguments)
                .into_iter()
                .map(|(name, value)| (name, normalize_expr(&value, &environment.expressions)))
                .collect(),
            span: expression.span.clone(),
        })
    }

    fn validate_run_call(
        &mut self,
        item: &ItemRef,
        expression: &Expr,
        run: &RunTemplate,
        environment: &TypeEnv,
    ) {
        if let ExprKind::Call { callee, .. } = &expression.kind
            && matches!(&callee.kind, ExprKind::Call { .. })
        {
            self.validate_configuration(item, &run.process, callee, &run.config, environment);
        }
        self.validate_invocation(item, &run.process, expression, &run.arguments, environment);
    }

    fn validate_configuration(
        &mut self,
        item: &ItemRef,
        process_name: &str,
        expression: &Expr,
        arguments: &IndexMap<String, Expr>,
        environment: &TypeEnv,
    ) {
        self.validate_unique_process_arguments(item, expression, "configuration");
        let members = self
            .resolved_members
            .get(process_name)
            .cloned()
            .unwrap_or_default();
        for (name, member) in members {
            if member.role != MemberRole::Config {
                continue;
            }
            if let Some(value) = arguments.get(&name) {
                let actual = self.infer_expr(&item.file_key, value, environment);
                if !self.types_compatible(&member.ty, &actual) && actual != Type::Error {
                    self.push_error(
                        &item.file_key,
                        value.span.clone(),
                        "E0621",
                        format!(
                            "configuration {process_name}.{name} expects {}, found {actual}",
                            member.ty
                        ),
                    );
                }
                if matches!(value.kind, ExprKind::Call { .. })
                    && let Some(configured) = self.configured_expression(value, environment)
                {
                    self.validate_configuration(
                        item,
                        &configured.process,
                        value,
                        &configured.config,
                        environment,
                    );
                }
            } else {
                self.push_obligation(
                    &item.file_key,
                    expression.span.clone(),
                    "missing_configuration",
                    format!("{process_name}.{name}"),
                    member.ty.to_string(),
                    format!("configured process is missing {name}"),
                    None,
                );
            }
        }
        for name in arguments.keys() {
            if !self
                .resolved_members
                .get(process_name)
                .is_some_and(|members| {
                    members
                        .get(name)
                        .is_some_and(|member| member.role == MemberRole::Config)
                })
            {
                self.push_error(
                    &item.file_key,
                    expression.span.clone(),
                    "E0622",
                    format!("unknown configuration argument {process_name}.{name}"),
                );
            }
        }
    }

    fn validate_invocation(
        &mut self,
        item: &ItemRef,
        process_name: &str,
        expression: &Expr,
        arguments: &IndexMap<String, Expr>,
        environment: &TypeEnv,
    ) {
        self.validate_unique_process_arguments(item, expression, "invocation");
        let Some(ItemRef {
            item: Item::Process(process),
            ..
        }) = self.processes.get(process_name).cloned()
        else {
            return;
        };
        let artifact_names = self.artifact_name_map();
        for parameter in &process.parameters {
            let expected = self
                .registry
                .resolve_type(&parameter.ty, &artifact_names, &[]);
            if let Some(value) = arguments.get(&parameter.name) {
                let actual = self.infer_expr(&item.file_key, value, environment);
                if !compatible(&expected, &actual) && actual != Type::Error {
                    self.push_error(
                        &item.file_key,
                        value.span.clone(),
                        "E0623",
                        format!(
                            "invocation {process_name}.{} expects {expected}, found {actual}",
                            parameter.name
                        ),
                    );
                }
            } else {
                self.push_obligation(
                    &item.file_key,
                    expression.span.clone(),
                    "missing_argument",
                    format!("{process_name}.{}", parameter.name),
                    expected.to_string(),
                    format!("process invocation is missing {}", parameter.name),
                    None,
                );
            }
        }
        for name in arguments.keys() {
            if !process
                .parameters
                .iter()
                .any(|parameter| &parameter.name == name)
            {
                self.push_error(
                    &item.file_key,
                    expression.span.clone(),
                    "E0624",
                    format!("unknown invocation argument {process_name}.{name}"),
                );
            }
        }
    }

    fn validate_unique_process_arguments(
        &mut self,
        item: &ItemRef,
        expression: &Expr,
        phase: &str,
    ) {
        let ExprKind::Call { arguments, .. } = &expression.kind else {
            return;
        };
        let mut seen = HashSet::new();
        for (index, argument) in arguments.iter().enumerate() {
            let name = argument
                .name
                .as_deref()
                .or_else(|| argument.value.as_name())
                .map(str::to_owned)
                .unwrap_or_else(|| format!("${index}"));
            if !seen.insert(name.clone()) {
                self.push_error(
                    &item.file_key,
                    argument.span.clone(),
                    "E0629",
                    format!("process {phase} argument {name} is supplied more than once"),
                );
            }
        }
    }

    fn extract_yield_template(
        &self,
        value: &Expr,
        environment: &TypeEnv,
        span: &Span,
    ) -> Option<YieldTemplate> {
        let ExprKind::Call { callee, arguments } = &value.kind else {
            return None;
        };
        let artifact = callee.as_name()?;
        if !self.artifacts.contains_key(artifact) {
            return None;
        }
        Some(YieldTemplate {
            artifact: artifact.into(),
            fields: arguments
                .iter()
                .filter_map(|argument| {
                    Some((
                        argument.name.clone()?,
                        normalize_expr(&argument.value, &environment.expressions),
                    ))
                })
                .collect(),
            span: span.clone(),
        })
    }

    fn consume_expr_owned(&mut self, item: &ItemRef, expression: &Expr, environment: &mut TypeEnv) {
        if let Some(name) = expression.as_name() {
            if environment.values.get(name).is_some_and(Type::is_owned) {
                match environment.ownership.get(name).copied() {
                    Some(PlaceState::Moved) => self.push_error(
                        &item.file_key,
                        expression.span.clone(),
                        "E0701",
                        format!("owned value {name} is moved more than once"),
                    ),
                    Some(PlaceState::Borrowed) => self.push_error(
                        &item.file_key,
                        expression.span.clone(),
                        "E0704",
                        format!("borrowed resource {name} cannot be moved or exported"),
                    ),
                    _ => {
                        environment.ownership.insert(name.into(), PlaceState::Moved);
                    }
                }
            }
            return;
        }
        if let ExprKind::Call { callee, arguments } = &expression.kind
            && matches!(
                callee.path().as_deref(),
                Some("std.stream.zip" | "std.stream.zip_exact")
            )
        {
            for argument in arguments {
                self.consume_expr_owned(item, &argument.value, environment);
            }
            return;
        }
        if let ExprKind::Call { callee, arguments } = &expression.kind
            && callee
                .as_name()
                .is_some_and(|name| self.artifacts.contains_key(name))
        {
            let mut tuple_roots = HashMap::<String, (HashSet<String>, Span)>::new();
            for argument in arguments {
                if let ExprKind::Member { value, member } = &argument.value.kind
                    && let Some(root) = value.as_name()
                    && environment.values.get(root).is_some_and(|ty| {
                        matches!(ty, Type::NamedTuple(fields) if fields.iter().any(|(name, ty)| name == member && ty.is_owned()))
                    })
                {
                    let (members, _) = tuple_roots
                        .entry(root.to_owned())
                        .or_insert_with(|| (HashSet::new(), value.span.clone()));
                    if !members.insert(member.clone()) {
                        self.push_error(
                            &item.file_key,
                            argument.value.span.clone(),
                            "E0701",
                            format!("owned tuple field {root}.{member} is moved more than once"),
                        );
                    }
                    continue;
                }
                self.consume_expr_owned(item, &argument.value, environment);
            }
            for (root, (_, span)) in tuple_roots {
                self.consume_expr_owned(item, &Expr::name(root, span), environment);
            }
            return;
        }
        if let ExprKind::Array { items } = &expression.kind {
            for item_expression in items {
                self.consume_expr_owned(item, item_expression, environment);
            }
        }
        if let ExprKind::Index { value, .. } | ExprKind::Member { value, .. } = &expression.kind
            && self
                .place_type(expression, environment)
                .is_some_and(|ty| ty.is_owned())
        {
            // Until subplace states are represented explicitly, moving an owned child
            // conservatively moves its root. This rejects some valid sibling extraction,
            // but never permits the same physical child to be transferred twice.
            self.consume_expr_owned(item, value, environment);
        }
    }

    fn mark_consumed_run_exports(
        &self,
        expression: &Expr,
        environment: &TypeEnv,
        summary: &mut ProcessSummary,
    ) {
        match &expression.kind {
            ExprKind::Member { value, member } => {
                if let Some(run_name) = value.as_name()
                    && let Some(run) = environment.runs.get(run_name)
                    && self
                        .resolved_members
                        .get(&run.process)
                        .and_then(|members| members.get(member))
                        .is_some_and(|member| member.ty.is_owned())
                {
                    summary
                        .consumed_run_exports
                        .insert(format!("{run_name}.{member}"));
                    return;
                }
                self.mark_consumed_run_exports(value, environment, summary);
            }
            ExprKind::Presentation { value, .. }
            | ExprKind::Index { value, .. }
            | ExprKind::Unary { value, .. } => {
                self.mark_consumed_run_exports(value, environment, summary);
            }
            ExprKind::Binary { left, right, .. } => {
                self.mark_consumed_run_exports(left, environment, summary);
                self.mark_consumed_run_exports(right, environment, summary);
            }
            ExprKind::Call { arguments, .. } => {
                for argument in arguments {
                    self.mark_consumed_run_exports(&argument.value, environment, summary);
                }
            }
            ExprKind::Array { items } => {
                for item in items {
                    self.mark_consumed_run_exports(item, environment, summary);
                }
            }
            ExprKind::Lambda { body, .. } => {
                self.mark_consumed_run_exports(body, environment, summary);
            }
            _ => {}
        }
    }

    fn place_type(&self, expression: &Expr, environment: &TypeEnv) -> Option<Type> {
        match &expression.kind {
            ExprKind::Name { value } => environment.values.get(value).cloned(),
            ExprKind::Index { value, .. } => match self.place_type(value, environment)? {
                Type::Array { element, .. } | Type::Stream(element) => Some(*element),
                _ => None,
            },
            ExprKind::Member { value, member } => match self.place_type(value, environment)? {
                Type::Artifact(artifact) => self
                    .model
                    .artifacts
                    .get(&artifact)
                    .and_then(|model| model.fields.get(member))
                    .cloned(),
                Type::NamedTuple(fields) => fields
                    .into_iter()
                    .find_map(|(name, ty)| (name == *member).then_some(ty)),
                Type::Array { element, size } => match *element {
                    Type::Artifact(artifact) => self
                        .model
                        .artifacts
                        .get(&artifact)
                        .and_then(|model| model.fields.get(member))
                        .cloned()
                        .map(|field| Type::Array {
                            element: Box::new(field),
                            size,
                        }),
                    _ => None,
                },
                _ => None,
            },
            _ => None,
        }
    }

    fn validate_process_graph(
        &mut self,
        item: &ItemRef,
        process: &Process,
        members: &IndexMap<String, ResolvedMember>,
        environment: &mut TypeEnv,
        summary: &mut ProcessSummary,
    ) {
        let mut connected_targets = HashSet::<String>::new();
        let mut consumed_sources = summary
            .consumed_run_exports
            .iter()
            .cloned()
            .collect::<HashSet<_>>();
        let effect_model_sources = effect_wire_sources(&summary.effects);
        let mut model_wires = Vec::new();
        for (source, target, span) in summary.wires.clone() {
            let source_path = source.path().unwrap_or_else(|| source.display());
            let target_path = target.path().unwrap_or_else(|| target.display());
            let model_source_path = effect_model_sources
                .get(&target_path)
                .cloned()
                .unwrap_or_else(|| source_path.clone());
            let source_type = self.endpoint_type(process, summary, members, &source, true);
            let target_type = self.endpoint_type(process, summary, members, &target, false);
            let source_role = self.endpoint_role(summary, members, &source);
            let target_role = self.endpoint_role(summary, members, &target);
            let source_is_child = matches!(&source.kind, ExprKind::Member { value, .. } if value.as_name().is_some_and(|name| summary.runs.contains_key(name)));
            let target_is_child = matches!(&target.kind, ExprKind::Member { value, .. } if value.as_name().is_some_and(|name| summary.runs.contains_key(name)));
            if source_type == Type::Unknown {
                self.push_error(
                    &item.file_key,
                    source.span.clone(),
                    "E0706",
                    format!("unknown wire source {source_path}"),
                );
            }
            if target_type == Type::Unknown {
                self.push_error(
                    &item.file_key,
                    target.span.clone(),
                    "E0707",
                    format!("unknown wire target {target_path}"),
                );
            }
            let valid_source = if source_is_child {
                source_role == Some(MemberRole::Export)
            } else {
                matches!(source_role, Some(MemberRole::Gets | MemberRole::Uses))
            };
            if source_role.is_some() && !valid_source {
                self.push_error(
                    &item.file_key,
                    source.span.clone(),
                    "E0708",
                    format!("{source_path} cannot be used as a wire source"),
                );
            }
            let valid_target = if target_is_child {
                matches!(target_role, Some(MemberRole::Gets | MemberRole::Uses))
            } else {
                target_role == Some(MemberRole::Export)
            };
            if target_role.is_some() && !valid_target {
                self.push_error(
                    &item.file_key,
                    target.span.clone(),
                    "E0709",
                    format!("{target_path} cannot be used as a wire target"),
                );
            }
            if !source_is_child
                && target_is_child
                && source_role == Some(MemberRole::Uses)
                && target_role == Some(MemberRole::Gets)
            {
                self.push_error(
                    &item.file_key,
                    span.clone(),
                    "E0710",
                    format!(
                        "borrowed source {source_path} cannot satisfy ownership-transfer input {target_path}"
                    ),
                );
            }
            if !compatible(&source_type, &target_type) {
                self.push_error(
                    &item.file_key,
                    span.clone(),
                    "E0702",
                    format!(
                        "cannot wire {source_path}: {source_type} to {target_path}: {target_type}"
                    ),
                );
            }
            if !connected_targets.insert(target_path.clone()) {
                self.push_error(
                    &item.file_key,
                    span.clone(),
                    "E0703",
                    format!("target {target_path} has more than one source"),
                );
            }
            if let Some(target_name) = target.as_name()
                && members
                    .get(target_name)
                    .is_some_and(|member| member.role == MemberRole::Export)
            {
                summary.assigned_exports.insert(target_name.to_owned());
            }
            let moves = source_type.is_owned() && target_role != Some(MemberRole::Uses);
            let loop_carried_local = source
                .as_name()
                .is_some_and(|name| summary.effect_mutable_bindings.contains(name));
            if moves && !loop_carried_local && !consumed_sources.insert(source_path.clone()) {
                self.push_error(
                    &item.file_key,
                    span.clone(),
                    "E0704",
                    format!("owned source {source_path} is wired more than once"),
                );
            }
            if moves && let Some(source_name) = source.as_name() {
                if members
                    .get(source_name)
                    .is_some_and(|member| member.role == MemberRole::Gets && member.ty.is_owned())
                {
                    summary.surviving_inputs.insert(source_name.to_owned());
                }
                let effect_local = summary.effect_binding_types.contains_key(source_name)
                    && !members.contains_key(source_name);
                if !effect_local
                    && environment.ownership.get(source_name) == Some(&PlaceState::Moved)
                {
                    self.push_error(
                        &item.file_key,
                        source.span.clone(),
                        "E0712",
                        format!("owned value {source_name} was already moved before wiring"),
                    );
                }
                if !effect_local {
                    environment
                        .ownership
                        .insert(source_name.to_owned(), PlaceState::Moved);
                }
            }
            let ownership = if target_role == Some(MemberRole::Uses) {
                "loan"
            } else if matches!(source_type, Type::Resource { .. }) {
                "resource_alias"
            } else if source_type.is_owned() {
                "move"
            } else {
                "copy"
            };
            model_wires.push(WireModel {
                source: model_source_path,
                target: target_path,
                ty: source_type.to_string(),
                ownership: ownership.into(),
            });
        }

        for (run_name, run) in &summary.runs {
            let child_members = self
                .resolved_members
                .get(&run.process)
                .cloned()
                .unwrap_or_default();
            for (port, member) in child_members {
                let endpoint = format!("{run_name}.{port}");
                match member.role {
                    MemberRole::Gets | MemberRole::Uses
                        if !connected_targets.contains(&endpoint) =>
                    {
                        self.push_obligation(
                            &item.file_key,
                            run.span.clone(),
                            if member.role == MemberRole::Gets {
                                "unconnected_gets"
                            } else {
                                "unconnected_uses"
                            },
                            format!("{}.{}", process.name, endpoint),
                            member.ty.to_string(),
                            format!("run input {endpoint} has no source"),
                            None,
                        );
                    }
                    MemberRole::Export
                        if member.ty.is_owned() && !consumed_sources.contains(&endpoint) =>
                    {
                        self.push_obligation(
                            &item.file_key,
                            run.span.clone(),
                            "dangling_owned_export",
                            format!("{}.{}", process.name, endpoint),
                            member.ty.to_string(),
                            format!("owned child output {endpoint} has no destination"),
                            None,
                        );
                    }
                    _ => {}
                }
            }
        }
        for (first, second) in &summary.ordering {
            if !summary.runs.contains_key(first) || !summary.runs.contains_key(second) {
                self.push_error(
                    &item.file_key,
                    process.span.clone(),
                    "E0705",
                    format!("ordering edge references unknown run: {first} before {second}"),
                );
            }
        }
        if let Some(model) = self.model.processes.get_mut(&process.name) {
            model.wires = model_wires;
        }
    }

    /// Best-effort type of one end of a wire.
    ///
    /// Returns [`Type::Unknown`] when the endpoint names nothing this process
    /// can see. That is the "not inferred" case, not the "already reported"
    /// case: `check_wires` turns it into E0706/E0707.
    fn endpoint_type(
        &self,
        process: &Process,
        summary: &ProcessSummary,
        members: &IndexMap<String, ResolvedMember>,
        expression: &Expr,
        source: bool,
    ) -> Type {
        if let Some(name) = expression.as_name() {
            return members
                .get(name)
                .map(|member| member.ty.clone())
                .or_else(|| summary.effect_binding_types.get(name).cloned())
                .unwrap_or(Type::Unknown);
        }
        if let ExprKind::Member { value, member } = &expression.kind
            && let Some(run_name) = value.as_name()
            && let Some(run) = summary.runs.get(run_name)
        {
            return self
                .resolved_members
                .get(&run.process)
                .and_then(|members| members.get(member))
                .map(|member| member.ty.clone())
                .unwrap_or(Type::Unknown);
        }
        let _ = (process, source);
        Type::Unknown
    }

    fn endpoint_role(
        &self,
        summary: &ProcessSummary,
        members: &IndexMap<String, ResolvedMember>,
        expression: &Expr,
    ) -> Option<MemberRole> {
        if let Some(name) = expression.as_name() {
            return members.get(name).map(|member| member.role);
        }
        if let ExprKind::Member { value, member } = &expression.kind {
            let run_name = value.as_name()?;
            let run = summary.runs.get(run_name)?;
            return self
                .resolved_members
                .get(&run.process)
                .and_then(|members| members.get(member))
                .map(|member| member.role);
        }
        None
    }

    fn finish_exports(
        &mut self,
        item: &ItemRef,
        process: &Process,
        members: &IndexMap<String, ResolvedMember>,
        summary: &mut ProcessSummary,
    ) {
        let composite = !summary.runs.is_empty();
        for (name, member) in members {
            if member.role != MemberRole::Export {
                continue;
            }
            if matches!(member.ty, Type::Stream(_)) {
                summary.assigned_exports.insert(name.clone());
                continue;
            }
            let Some(rule) = member.aggregate else {
                if summary.assigned_exports.contains(name) {
                    continue;
                }
                self.push_obligation(
                    &item.file_key,
                    process.span.clone(),
                    "missing_export",
                    format!("{}.{}", process.name, name),
                    member.ty.to_string(),
                    format!("process body does not define export {name}"),
                    None,
                );
                continue;
            };

            // A local assignment replaces an inherited fallback. It is always
            // the process's direct contribution; the externally visible export
            // is derived below from that contribution and direct-child totals.
            if summary.assigned_exports.contains(name)
                && let Some(value) = summary.export_values.get(name).cloned()
            {
                summary.direct_export_values.insert(name.clone(), value);
            }
            let has_direct = summary.direct_export_values.contains_key(name);
            if rule == AggregateRule::Explicit {
                if has_direct {
                    summary.assigned_exports.insert(name.clone());
                    if let Some(value) = summary.direct_export_values.get(name).cloned() {
                        summary.export_values.insert(name.clone(), value);
                    }
                    continue;
                }
            } else if composite {
                summary.assigned_exports.insert(name.clone());
                self.default_aggregate(item, process, summary, name, member);
                continue;
            } else if has_direct {
                summary.assigned_exports.insert(name.clone());
                if let Some(value) = summary.direct_export_values.get(name).cloned() {
                    summary.export_values.insert(name.clone(), value);
                }
                continue;
            }
            self.push_obligation(
                &item.file_key,
                process.span.clone(),
                "missing_export",
                format!("{}.{}", process.name, name),
                member.ty.to_string(),
                format!("process body does not define export {name}"),
                None,
            );
        }
    }

    fn default_aggregate(
        &mut self,
        item: &ItemRef,
        process: &Process,
        summary: &mut ProcessSummary,
        export: &str,
        member: &ResolvedMember,
    ) {
        let rule = member
            .aggregate
            .expect("default aggregation requires an aggregate rule");
        let run_multiplicities = effect_run_multiplicities(summary, &process.span);
        let mut contributors = IndexMap::new();
        for (run_name, run) in &summary.runs {
            let child_member = self
                .resolved_members
                .get(&run.process)
                .and_then(|members| members.get(export));
            let contributes = child_member.is_some_and(|child| {
                child.role == MemberRole::Export && compatible(&member.ty, &child.ty)
            });
            if contributes {
                let contribution = Expr {
                    kind: ExprKind::Member {
                        value: Box::new(Expr::name(run_name, process.span.clone())),
                        member: export.to_owned(),
                    },
                    span: process.span.clone(),
                };
                let contribution = if rule == AggregateRule::Sum {
                    run_multiplicities
                        .get(run_name)
                        .map_or(contribution.clone(), |count| {
                            binary_expression(
                                BinaryOp::Mul,
                                contribution,
                                count.clone(),
                                process.span.clone(),
                            )
                        })
                } else {
                    contribution
                };
                contributors.insert(run_name.clone(), contribution);
            } else {
                summary.metric_coverage_gaps.push(MetricCoverageGap {
                    run: run_name.clone(),
                    metric: export.to_owned(),
                    expected: member.ty.clone(),
                    process: run.process.clone(),
                    rule,
                    span: run.span.clone(),
                });
            }
        }
        let direct = summary.direct_export_values.get(export).cloned();
        let aggregate_expression = match rule {
            AggregateRule::Makespan => {
                let child_total =
                    self.child_makespan_expression(process, summary, member, &contributors);
                match direct {
                    Some(direct) => {
                        call_expression("sum", vec![direct, child_total], process.span.clone())
                    }
                    None => child_total,
                }
            }
            AggregateRule::Sum | AggregateRule::Max | AggregateRule::Min => {
                let mut values = direct.into_iter().collect::<Vec<_>>();
                values.extend(contributors.into_values());
                if values.is_empty() {
                    self.zero_expression(&member.ty, process.span.clone())
                } else {
                    call_expression(rule.as_str(), values, process.span.clone())
                }
            }
            AggregateRule::Explicit => unreachable!("explicit exports are not aggregated"),
        };
        let expression = aggregate_expression.display();
        summary
            .export_values
            .insert(export.to_owned(), aggregate_expression);
        let span = self.span(&item.file_key, process.span.clone());
        let phase = if rule == AggregateRule::Makespan {
            "scheduling"
        } else {
            "completion"
        };
        self.report.constraints.push(Constraint {
            id: format!(
                "constraint:{}",
                stable_fingerprint(&format!("{}:{export}", process.name))
            ),
            path: format!("{}.{phase}.aggregate[{export}]", process.name),
            phase: phase.into(),
            predicate: format!("{export} == {expression}"),
            status: "residual".into(),
            span,
            dependencies: summary.runs.keys().cloned().collect(),
        });
    }

    fn child_makespan_expression(
        &self,
        process: &Process,
        summary: &ProcessSummary,
        member: &ResolvedMember,
        contributors: &IndexMap<String, Expr>,
    ) -> Expr {
        if contributors.is_empty() {
            return self.zero_expression(&member.ty, process.span.clone());
        }

        // A core stream edge allows producer and consumer execution to overlap.
        // Without per-item latency/rate information the exact event makespan is
        // intentionally residual rather than being approximated as a serial edge.
        let has_stream_edge = self.has_child_stream_edge(process, summary);
        let mut loan_users = IndexMap::<String, Vec<String>>::new();
        for (source, target, _) in &summary.wires {
            let Some(run_name) = child_run_name(target, summary) else {
                continue;
            };
            let ExprKind::Member { member: port, .. } = &target.kind else {
                continue;
            };
            let is_loan = summary
                .runs
                .get(run_name)
                .and_then(|run| self.resolved_members.get(&run.process))
                .and_then(|members| members.get(port))
                .is_some_and(|member| member.role == MemberRole::Uses);
            if is_loan {
                loan_users
                    .entry(source.display())
                    .or_default()
                    .push(run_name.to_owned());
            }
        }
        let ordering = summary.ordering.iter().fold(
            HashMap::<String, BTreeSet<String>>::new(),
            |mut graph, edge| {
                graph
                    .entry(edge.0.clone())
                    .or_default()
                    .insert(edge.1.clone());
                graph
            },
        );
        let has_unordered_loan = loan_users.values().any(|users| {
            (0..users.len()).any(|left| {
                (left + 1..users.len()).any(|right| {
                    !is_ordered(&users[left], &users[right], &ordering)
                        && !is_ordered(&users[right], &users[left], &ordering)
                })
            })
        });
        if has_stream_edge || has_unordered_loan {
            return call_expression(
                "makespan",
                contributors.values().cloned().collect(),
                process.span.clone(),
            );
        }

        let mut predecessors = summary
            .runs
            .keys()
            .map(|run| (run.clone(), BTreeSet::new()))
            .collect::<IndexMap<_, _>>();
        for (first, second) in &summary.ordering {
            if predecessors.contains_key(first)
                && let Some(values) = predecessors.get_mut(second)
            {
                values.insert(first.clone());
            }
        }
        for (source, target, _) in &summary.wires {
            let (Some(first), Some(second)) = (
                child_run_name(source, summary),
                child_run_name(target, summary),
            ) else {
                continue;
            };
            if let Some(values) = predecessors.get_mut(second) {
                values.insert(first.to_owned());
            }
        }

        fn finish_time(
            run: &str,
            predecessors: &IndexMap<String, BTreeSet<String>>,
            contributors: &IndexMap<String, Expr>,
            zero: &Expr,
            span: &Span,
            memo: &mut HashMap<String, Expr>,
            visiting: &mut BTreeSet<String>,
        ) -> Option<Expr> {
            if let Some(value) = memo.get(run) {
                return Some(value.clone());
            }
            if !visiting.insert(run.to_owned()) {
                return None;
            }
            let duration = contributors
                .get(run)
                .cloned()
                .unwrap_or_else(|| zero.clone());
            let prior = predecessors
                .get(run)
                .into_iter()
                .flatten()
                .map(|predecessor| {
                    finish_time(
                        predecessor,
                        predecessors,
                        contributors,
                        zero,
                        span,
                        memo,
                        visiting,
                    )
                })
                .collect::<Option<Vec<_>>>()?;
            visiting.remove(run);
            let value = if prior.is_empty() {
                duration
            } else {
                call_expression(
                    "sum",
                    vec![call_expression("max", prior, span.clone()), duration],
                    span.clone(),
                )
            };
            memo.insert(run.to_owned(), value.clone());
            Some(value)
        }

        let zero = self.zero_expression(&member.ty, process.span.clone());
        let mut memo = HashMap::new();
        let mut visiting = BTreeSet::new();
        let finishes = summary
            .runs
            .keys()
            .map(|run| {
                finish_time(
                    run,
                    &predecessors,
                    contributors,
                    &zero,
                    &process.span,
                    &mut memo,
                    &mut visiting,
                )
            })
            .collect::<Option<Vec<_>>>();
        finishes.map_or_else(
            || {
                call_expression(
                    "makespan",
                    contributors.values().cloned().collect(),
                    process.span.clone(),
                )
            },
            |values| call_expression("max", values, process.span.clone()),
        )
    }

    fn has_child_stream_edge(&self, process: &Process, summary: &ProcessSummary) -> bool {
        summary.wires.iter().any(|(source, target, _)| {
            child_run_name(source, summary).is_some()
                && child_run_name(target, summary).is_some()
                && matches!(
                    self.endpoint_type(process, summary, &IndexMap::new(), source, true),
                    Type::Stream(_)
                )
        })
    }

    fn process_graph_has_stream_edge(
        &self,
        process_name: &str,
        configured_bindings: &IndexMap<String, ConfiguredTemplate>,
        visiting: &mut BTreeSet<String>,
    ) -> bool {
        if !visiting.insert(process_name.to_owned()) {
            return false;
        }
        let result = self
            .summaries
            .get(process_name)
            .zip(self.processes.get(process_name))
            .is_some_and(|(summary, item)| {
                let Item::Process(process) = &item.item else {
                    return false;
                };
                self.has_child_stream_edge(process, summary)
                    || summary.runs.values().any(|run| {
                        let run = resolve_run_template(run, configured_bindings);
                        self.process_graph_has_stream_edge(
                            &run.process,
                            &run.configured_arguments,
                            visiting,
                        )
                    })
            });
        visiting.remove(process_name);
        result
    }

    fn publish_process_model(&mut self, process: &Process, summary: &ProcessSummary) {
        if let Some(model) = self.model.processes.get_mut(&process.name) {
            model.composite = !summary.runs.is_empty();
            model.runs = summary
                .runs
                .iter()
                .map(|(name, run)| {
                    (
                        name.clone(),
                        RunModel {
                            process: run.process.clone(),
                            process_source: run.process_source.clone(),
                            configured_by: run.configured_by.clone(),
                            config: run
                                .config
                                .iter()
                                .map(|(name, value)| (name.clone(), value.display()))
                                .collect(),
                            config_sources: run.config_sources.clone(),
                            configured_arguments: run
                                .configured_arguments
                                .iter()
                                .map(|(name, configured)| {
                                    (name.clone(), configured_process_model(configured))
                                })
                                .collect(),
                            arguments: run
                                .arguments
                                .iter()
                                .map(|(name, value)| (name.clone(), value.display()))
                                .collect(),
                        },
                    )
                })
                .collect();
            model.ordering = summary.ordering.clone();
            model.aggregate_contributions = summary
                .direct_export_values
                .iter()
                .map(|(name, value)| (name.clone(), value.display()))
                .collect();
        }
    }

    fn publish_mutations(&mut self, item: &ItemRef, process: &Process, summary: &ProcessSummary) {
        for (index, mutation) in summary.mutations.iter().enumerate() {
            self.report.provenance.push(ProvenanceRecord {
                id: format!(
                    "state:{}",
                    stable_fingerprint(&format!("{}:{index}", process.name))
                ),
                kind: "state_transition".into(),
                artifact: mutation.artifact.clone(),
                run: process.name.clone(),
                sources: vec![
                    format!("before={}", mutation.before),
                    format!("after={}", mutation.after),
                    format!("field={}", mutation.field),
                ],
                span: self.span(&item.file_key, mutation.span.clone()),
            });
        }
    }

    fn record_artifact_production(
        &mut self,
        item: &ItemRef,
        process: &Process,
        export: &str,
        value: &Expr,
        environment: &TypeEnv,
    ) {
        let Some(production) = self.artifact_production_expr(value, environment) else {
            return;
        };
        let ExprKind::Call { callee, .. } = &production.kind else {
            unreachable!("artifact production expressions are constructor calls");
        };
        let Some(artifact) = callee.as_name() else {
            return;
        };
        if !self.artifacts.contains_key(artifact) {
            return;
        }
        let sources = self
            .resolved_members
            .get(&process.name)
            .into_iter()
            .flat_map(|members| members.iter())
            .filter(|(_, member)| member.role == MemberRole::Gets)
            .map(|(name, _)| name.clone())
            .collect();
        self.report.provenance.push(ProvenanceRecord {
            id: format!(
                "artifact:{}",
                stable_fingerprint(&format!(
                    "{}:{export}:{}",
                    process.name, production.span.start
                ))
            ),
            kind: "produced".into(),
            artifact: format!("{}.{}:{artifact}", process.name, export),
            run: process.name.clone(),
            sources,
            span: self.span(&item.file_key, production.span.clone()),
        });
    }

    fn artifact_production_expr(&self, value: &Expr, environment: &TypeEnv) -> Option<Expr> {
        match &value.kind {
            ExprKind::Call { callee, .. }
                if callee
                    .as_name()
                    .is_some_and(|name| self.artifacts.contains_key(name)) =>
            {
                Some(value.clone())
            }
            ExprKind::Name { value } => environment.production_exprs.get(value).cloned(),
            _ => None,
        }
    }

    fn mark_surviving_inputs(
        &self,
        process: &Process,
        value: &Expr,
        environment: &TypeEnv,
        summary: &mut ProcessSummary,
    ) {
        let Some(members) = self.resolved_members.get(&process.name) else {
            return;
        };
        for name in self.owned_lineage(value, environment) {
            if members
                .get(&name)
                .is_some_and(|member| member.role == MemberRole::Gets && member.ty.is_owned())
            {
                summary.surviving_inputs.insert(name);
            }
        }
    }

    fn owned_lineage(&self, value: &Expr, environment: &TypeEnv) -> BTreeSet<String> {
        match &value.kind {
            ExprKind::Name { value } => environment.lineage.get(value).cloned().unwrap_or_default(),
            ExprKind::Array { items } => items
                .iter()
                .flat_map(|item| self.owned_lineage(item, environment))
                .collect(),
            ExprKind::Call { callee, arguments } => {
                let mut lineage = BTreeSet::new();
                if matches!(
                    callee.path().as_deref(),
                    Some("std.stream.zip" | "std.stream.zip_exact")
                ) {
                    for argument in arguments {
                        lineage.extend(self.owned_lineage(&argument.value, environment));
                    }
                    return lineage;
                }
                if let Some(artifact) = callee
                    .as_name()
                    .and_then(|name| self.model.artifacts.get(name))
                {
                    for argument in arguments {
                        let Some(field) = argument.name.as_ref() else {
                            continue;
                        };
                        if artifact.fields.get(field).is_some_and(Type::is_owned) {
                            lineage.extend(self.owned_lineage(&argument.value, environment));
                        }
                    }
                }
                lineage
            }
            ExprKind::Member { value, .. } | ExprKind::Index { value, .. } => {
                self.owned_lineage(value, environment)
            }
            _ => BTreeSet::new(),
        }
    }

    fn record_consumed_inputs(
        &mut self,
        item: &ItemRef,
        process: &Process,
        members: &IndexMap<String, ResolvedMember>,
        summary: &ProcessSummary,
    ) {
        for (name, member) in members {
            if member.role != MemberRole::Gets
                || !member.ty.is_owned()
                || summary.surviving_inputs.contains(name)
            {
                continue;
            }
            self.report.provenance.push(ProvenanceRecord {
                id: format!(
                    "consumed:{}",
                    stable_fingerprint(&format!("{}:{name}", process.name))
                ),
                kind: "consumed_by".into(),
                artifact: format!("{}.{}:{}", process.name, name, member.ty),
                run: process.name.clone(),
                sources: Vec::new(),
                span: self.span(&item.file_key, member.span.clone()),
            });
        }
    }

    fn validate_process_instantiation_cycles(&mut self) {
        #[derive(Clone)]
        struct RunEdge {
            child: String,
            file_key: String,
            span: Span,
        }

        fn visit(
            process: &str,
            graph: &IndexMap<String, Vec<RunEdge>>,
            states: &mut HashMap<String, u8>,
            stack: &mut Vec<String>,
            cycles: &mut Vec<(String, Span, Vec<String>)>,
        ) {
            states.insert(process.to_owned(), 1);
            stack.push(process.to_owned());
            if let Some(edges) = graph.get(process) {
                for edge in edges {
                    match states.get(&edge.child).copied().unwrap_or(0) {
                        0 => visit(&edge.child, graph, states, stack, cycles),
                        1 => {
                            let start = stack
                                .iter()
                                .position(|name| name == &edge.child)
                                .unwrap_or(0);
                            let mut cycle = stack[start..].to_vec();
                            cycle.push(edge.child.clone());
                            cycles.push((edge.file_key.clone(), edge.span.clone(), cycle));
                        }
                        _ => {}
                    }
                }
            }
            stack.pop();
            states.insert(process.to_owned(), 2);
        }

        let graph = self
            .summaries
            .iter()
            .map(|(parent, summary)| {
                let file_key = self
                    .processes
                    .get(parent)
                    .map(|item| item.file_key.clone())
                    .unwrap_or_default();
                let edges = summary
                    .runs
                    .values()
                    .map(|template| RunEdge {
                        child: template.process.clone(),
                        file_key: file_key.clone(),
                        span: template.span.clone(),
                    })
                    .collect();
                (parent.clone(), edges)
            })
            .collect::<IndexMap<_, _>>();
        let mut states = HashMap::new();
        let mut stack = Vec::new();
        let mut cycles = Vec::new();
        for process in graph.keys() {
            if states.get(process).copied().unwrap_or(0) == 0 {
                visit(process, &graph, &mut states, &mut stack, &mut cycles);
            }
        }
        for (file_key, span, cycle) in cycles {
            self.push_error(
                &file_key,
                span,
                "E0715",
                format!(
                    "process instantiation cycle has no finite run graph: {}",
                    cycle.join(" -> ")
                ),
            );
        }
    }

    fn validate_loan_scheduling(&mut self) {
        let composites = self
            .model
            .processes
            .iter()
            .filter(|(_, process)| process.composite)
            .map(|(name, process)| (name.clone(), process.clone()))
            .collect::<Vec<_>>();
        for (process_name, process) in composites {
            let Some(item) = self.processes.get(&process_name).cloned() else {
                continue;
            };
            let mut graph = HashMap::<String, BTreeSet<String>>::new();
            for (first, second) in &process.ordering {
                graph
                    .entry(first.clone())
                    .or_default()
                    .insert(second.clone());
            }
            for (first, second) in &process.ordering {
                if first == second || reaches(first, second, &graph, &mut HashSet::new()) {
                    self.push_error(
                        &item.file_key,
                        item.item.span().clone(),
                        "E0713",
                        format!("causal ordering in {process_name} contains a cycle involving {first} and {second}"),
                    );
                }
            }
            let mut loans = IndexMap::<String, Vec<(String, String)>>::new();
            for wire in &process.wires {
                if wire.ownership != "loan" {
                    continue;
                }
                let Some((run, port)) = wire.target.split_once('.') else {
                    continue;
                };
                loans
                    .entry(wire.source.clone())
                    .or_default()
                    .push((run.to_owned(), port.to_owned()));
            }
            for (source, borrowers) in loans {
                // Capacity resources are divisible pools. Their overlapping
                // claims are checked by the finite-resource scheduler rather
                // than by the exclusive whole-object loan rule used for
                // ordinary reusable values.
                let source_is_capacity_resource = process
                    .members
                    .get(&source)
                    .is_some_and(|member| matches!(member.ty, Type::Resource { .. }))
                    || source.split_once('.').is_some_and(|(run, port)| {
                        process
                            .runs
                            .get(run)
                            .and_then(|run| self.model.processes.get(&run.process))
                            .and_then(|process| process.members.get(port))
                            .is_some_and(|member| matches!(member.ty, Type::Resource { .. }))
                    });
                if source_is_capacity_resource {
                    continue;
                }
                for left_index in 0..borrowers.len() {
                    for right_index in left_index + 1..borrowers.len() {
                        let (left_run, left_port) = &borrowers[left_index];
                        let (right_run, right_port) = &borrowers[right_index];
                        let ordered = is_ordered(left_run, right_run, &graph)
                            || is_ordered(right_run, left_run, &graph);
                        if ordered {
                            continue;
                        }
                        let left_writes =
                            child_port_mutates(&process, &self.summaries, left_run, left_port);
                        let right_writes =
                            child_port_mutates(&process, &self.summaries, right_run, right_port);
                        if left_writes || right_writes {
                            self.push_error(
                                &item.file_key,
                                item.item.span().clone(),
                                "E0714",
                                format!(
                                    "exclusive borrowers {left_run} and {right_run} of {source} require an explicit before edge because at least one mutates it"
                                ),
                            );
                        } else {
                            let predicate =
                                format!("non_overlap({left_run}, {right_run}, resource: {source})");
                            self.report.constraints.push(Constraint {
                                id: format!(
                                    "constraint:{}",
                                    stable_fingerprint(&format!(
                                        "{process_name}:{source}:{left_run}:{right_run}:loan"
                                    ))
                                ),
                                path: format!(
                                    "{process_name}.scheduling.loan[{left_run},{right_run}]"
                                ),
                                phase: "scheduling".into(),
                                predicate,
                                status: "residual".into(),
                                span: self.span(&item.file_key, item.item.span().clone()),
                                dependencies: vec![
                                    source.clone(),
                                    left_run.clone(),
                                    right_run.clone(),
                                ],
                            });
                        }
                    }
                }
            }
        }
    }

    fn instantiate_child_constraints(&mut self) {
        // Requirements and scheduling constraints are rebuilt below from root
        // process instances so declaration templates are not duplicated beside
        // their concretely configured instances.
        let derived_templates = std::mem::take(&mut self.report.constraints);

        let mut invoked = BTreeSet::new();
        for summary in self.summaries.values() {
            for run in summary.runs.values() {
                invoked.insert(run.process.clone());
                collect_configured_template_processes(&run.configured_arguments, &mut invoked);
            }
        }
        for process_name in self.summaries.keys() {
            let configured = self.effective_root_configuration(process_name);
            collect_configured_template_processes(&configured.configured_arguments, &mut invoked);
        }
        let mut roots = self
            .summaries
            .keys()
            .filter(|name| {
                !invoked.contains(*name)
                    && self
                        .processes
                        .get(*name)
                        .is_some_and(|item| item.file_key == self.sources.entry_key)
            })
            .cloned()
            .collect::<Vec<_>>();
        if roots.is_empty() {
            roots.extend(self.summaries.keys().filter_map(|name| {
                self.processes
                    .get(name)
                    .filter(|item| item.file_key == self.sources.entry_key)
                    .map(|_| name.clone())
            }));
        }
        for root in &roots {
            let configured = self.effective_root_configuration(root);
            self.emit_process_constraints(
                root,
                root,
                &configured.config,
                &configured.configured_arguments,
                &HashMap::new(),
                &HashMap::new(),
                None,
                &mut Vec::new(),
            );
        }
        for root in &roots {
            let configured = self.effective_root_configuration(root);
            self.emit_derived_constraints(
                root,
                root,
                &configured.configured_arguments,
                &derived_templates,
                &mut Vec::new(),
            );
        }

        for root in &roots {
            let configured = self.effective_root_configuration(root);
            self.instantiate_child_obligations(
                root,
                root,
                &configured.config,
                &configured.configured_arguments,
                &IndexMap::new(),
                &integer_expression(1, 0..0),
                &mut Vec::new(),
            );
        }
        self.report.obligations.retain(|obligation| {
            roots.iter().any(|process| {
                obligation.path == *process
                    || obligation
                        .path
                        .strip_prefix(process)
                        .is_some_and(|suffix| suffix.starts_with('.'))
            })
        });
        self.project_stream_handoffs(&roots);
        self.project_provisioned_resources(&roots);
    }

    fn project_stream_handoffs(&mut self, roots: &[String]) {
        self.report.stream_handoffs.clear();
        self.report.stream_cardinalities.clear();
        for root in roots {
            let configured = self.effective_root_configuration(root);
            self.collect_stream_handoffs(
                root,
                root,
                &configured.config,
                &configured.configured_arguments,
                &mut Vec::new(),
            );
        }
        self.report
            .stream_handoffs
            .sort_by(|left, right| (&left.path, &left.stream).cmp(&(&right.path, &right.stream)));
        self.report
            .stream_handoffs
            .dedup_by(|left, right| left.path == right.path && left.stream == right.stream);
        self.report
            .stream_cardinalities
            .sort_by(|left, right| (&left.path, &left.stream).cmp(&(&right.path, &right.stream)));
        self.report
            .stream_cardinalities
            .dedup_by(|left, right| left.path == right.path && left.stream == right.stream);
    }

    fn project_provisioned_resources(&mut self, roots: &[String]) {
        self.resource_pool_bindings.clear();
        let mut drafts = Vec::new();
        let mut wires = Vec::new();
        for root in roots {
            let configured = self.effective_root_configuration(root);
            let summary = self.summaries.get(root).cloned().unwrap_or_default();
            let run_exports = self.instantiate_run_exports(
                &summary,
                &configured.configured_arguments,
                &configured.config,
            );
            let has_duration = self
                .resolved_members
                .get(root)
                .is_some_and(|members| members.contains_key("duration"));
            let completion = if has_duration {
                summary
                    .export_values
                    .get("duration")
                    .map(|duration| {
                        instantiate_expression(duration, &configured.config, &run_exports)
                    })
                    .and_then(|duration| self.constant_quantity(&duration))
                    .map(|(duration, _)| duration)
            } else {
                Some(BigRational::zero())
            };
            self.collect_resource_instance_graph(
                root,
                root,
                &configured.config,
                &configured.configured_arguments,
                &mut drafts,
                &mut wires,
                &mut Vec::new(),
                BigRational::zero(),
                completion,
            );
        }

        let mut outgoing = HashMap::<String, Vec<&ResourceWireDraft>>::new();
        for wire in &wires {
            outgoing.entry(wire.source.clone()).or_default().push(wire);
        }
        let mut projected = Vec::new();
        for draft in drafts {
            let constructed_capacity = self.constant_quantity(&draft.capacity);
            let mut aliases = BTreeSet::new();
            let mut borrowers = BTreeSet::new();
            let mut visited = BTreeSet::from([draft.path.clone()]);
            let mut queue = VecDeque::from([draft.path.clone()]);
            while let Some(endpoint) = queue.pop_front() {
                for wire in outgoing.get(&endpoint).into_iter().flatten() {
                    if wire.target_role == Some(MemberRole::Export) {
                        aliases.insert(wire.target.clone());
                    }
                    if wire.target_role == Some(MemberRole::Uses) {
                        borrowers.insert(wire.target.clone());
                        if let (Some((capacity, dimension)), Some((claim, claim_dimension))) =
                            (&constructed_capacity, &wire.target_claim)
                            && (dimension != claim_dimension || claim > capacity)
                        {
                            self.push_error(
                                &wire.file_key,
                                wire.span.clone(),
                                "E0727",
                                format!(
                                    "resource claim {} cannot be satisfied by provisioned pool {} with capacity {}",
                                    wire.target,
                                    draft.path,
                                    self.metric_value_display(
                                        &draft.capacity,
                                        &draft.capacity_type,
                                        None,
                                    )
                                ),
                            );
                        }
                    }
                    if visited.insert(wire.target.clone()) {
                        queue.push_back(wire.target.clone());
                    }
                }
            }
            if let Some((amount, dimension)) = constructed_capacity {
                for endpoint in &visited {
                    self.resource_pool_bindings.insert(
                        endpoint.clone(),
                        (
                            draft.path.clone(),
                            amount.clone(),
                            dimension.clone(),
                            draft.available_at.clone(),
                        ),
                    );
                }
            }
            let capacity = self.metric_value_display(&draft.capacity, &draft.capacity_type, None);
            projected.push(ProvisionedResource {
                path: draft.path,
                resource: draft.resource,
                capacity,
                available_after_seconds: draft
                    .available_at
                    .map(|value| format_exact_rational(&value)),
                aliases: aliases.into_iter().collect(),
                borrowers: borrowers.into_iter().collect(),
                utilization: None,
                span: self.span(&draft.file_key, draft.span),
            });
        }
        projected.sort_by(|left, right| left.path.cmp(&right.path));
        projected.dedup_by(|left, right| left.path == right.path);
        self.report.provisioned_resources = projected;
    }

    #[allow(clippy::too_many_arguments)]
    fn collect_resource_instance_graph(
        &self,
        process_name: &str,
        object_path: &str,
        substitutions: &IndexMap<String, Expr>,
        configured_bindings: &IndexMap<String, ConfiguredTemplate>,
        drafts: &mut Vec<ProvisionedResourceDraft>,
        wires: &mut Vec<ResourceWireDraft>,
        stack: &mut Vec<String>,
        object_start: BigRational,
        object_completion: Option<BigRational>,
    ) {
        if stack.iter().any(|name| name == process_name) {
            return;
        }
        let Some(summary) = self.summaries.get(process_name) else {
            return;
        };
        let Some(item) = self.processes.get(process_name) else {
            return;
        };
        let Some(members) = self.resolved_members.get(process_name) else {
            return;
        };
        stack.push(process_name.to_owned());
        let run_exports = self.instantiate_run_exports(summary, configured_bindings, substitutions);
        let can_complete = !self.report.obligations.iter().any(|obligation| {
            obligation.path == object_path
                || obligation
                    .path
                    .strip_prefix(object_path)
                    .is_some_and(|suffix| suffix.starts_with('.'))
        });
        for (export, constructor) in &summary.provisioned_resources {
            let Some(member) = members.get(export) else {
                continue;
            };
            let Type::Resource { name, capacity, .. } = &member.ty else {
                continue;
            };
            let Some(Type::Resource {
                claim: Some(claim), ..
            }) = self.resource_constructor(constructor)
            else {
                continue;
            };
            let Some(claim) = crate::parser::parse_expression(&claim).ok() else {
                continue;
            };
            drafts.push(ProvisionedResourceDraft {
                path: format!("{object_path}.{export}"),
                resource: name.clone(),
                capacity: instantiate_expression(&claim, substitutions, &run_exports),
                capacity_type: (**capacity).clone(),
                available_at: can_complete.then(|| object_completion.clone()).flatten(),
                file_key: item.file_key.clone(),
                span: constructor.span.clone(),
            });
        }

        let Item::Process(process) = &item.item else {
            stack.pop();
            return;
        };
        for (source_expression, target, _) in &summary.wires {
            let source_type =
                self.endpoint_type(process, summary, members, source_expression, true);
            if !matches!(source_type, Type::Resource { .. }) {
                continue;
            }
            let Some(source) = instance_endpoint_path(object_path, source_expression) else {
                continue;
            };
            let Some(target_path) = instance_endpoint_path(object_path, target) else {
                continue;
            };
            let target_role = self.endpoint_role(summary, members, target);
            let target_claim = (target_role == Some(MemberRole::Uses))
                .then(|| {
                    self.instantiated_resource_endpoint_amount(
                        summary,
                        members,
                        target,
                        substitutions,
                        configured_bindings,
                        &run_exports,
                    )
                })
                .flatten();
            wires.push(ResourceWireDraft {
                source,
                target: target_path,
                target_role,
                target_claim,
                file_key: item.file_key.clone(),
                span: target.span.clone(),
            });
        }

        let inputs = run_inputs(summary);
        let stream_handoffs =
            self.effect_stream_handoffs(summary, substitutions, configured_bindings, &run_exports);
        for (run_name, run) in &summary.runs {
            let run = resolve_run_template(run, configured_bindings);
            let mut child_substitutions = run
                .config
                .iter()
                .chain(&run.arguments)
                .map(|(name, value)| {
                    (
                        name.clone(),
                        instantiate_expression(value, substitutions, &run_exports),
                    )
                })
                .collect::<IndexMap<_, _>>();
            if let Some(child_members) = self.resolved_members.get(&run.process) {
                for (port, member) in child_members {
                    if matches!(member.role, MemberRole::Gets | MemberRole::Uses)
                        && let Some(source) = inputs.get(&(run_name.clone(), port.clone()))
                    {
                        child_substitutions.insert(
                            port.clone(),
                            instantiate_expression(source, substitutions, &run_exports),
                        );
                    }
                }
            }
            let timing = self.local_run_timing_for(
                run_name,
                summary,
                configured_bindings,
                &run_exports,
                &stream_handoffs,
            );
            let child_start = timing
                .as_ref()
                .map(|(start, _)| &object_start + start)
                .unwrap_or_else(|| object_start.clone());
            let child_completion = timing.map(|(_, finish)| &object_start + finish);
            self.collect_resource_instance_graph(
                &run.process,
                &format!("{object_path}.{run_name}"),
                &child_substitutions,
                &run.configured_arguments,
                drafts,
                wires,
                stack,
                child_start,
                child_completion,
            );
        }
        stack.pop();
    }

    fn instantiated_resource_endpoint_amount(
        &self,
        summary: &ProcessSummary,
        members: &IndexMap<String, ResolvedMember>,
        endpoint: &Expr,
        substitutions: &IndexMap<String, Expr>,
        configured_bindings: &IndexMap<String, ConfiguredTemplate>,
        run_exports: &HashMap<String, Expr>,
    ) -> Option<(BigRational, Dimension)> {
        if let Some(name) = endpoint.as_name() {
            return self.resource_amount(&members.get(name)?.ty, substitutions);
        }
        let ExprKind::Member { value, member } = &endpoint.kind else {
            return None;
        };
        let run = resolve_run_template(summary.runs.get(value.as_name()?)?, configured_bindings);
        let child_member = self.resolved_members.get(&run.process)?.get(member)?;
        let child_substitutions = run
            .config
            .iter()
            .chain(&run.arguments)
            .map(|(name, value)| {
                (
                    name.clone(),
                    instantiate_expression(value, substitutions, run_exports),
                )
            })
            .collect::<IndexMap<_, _>>();
        self.resource_amount(&child_member.ty, &child_substitutions)
    }

    fn collect_stream_handoffs(
        &mut self,
        process_name: &str,
        object_path: &str,
        substitutions: &IndexMap<String, Expr>,
        configured_bindings: &IndexMap<String, ConfiguredTemplate>,
        stack: &mut Vec<String>,
    ) {
        if stack.iter().any(|name| name == process_name) {
            return;
        }
        let Some(summary) = self.summaries.get(process_name).cloned() else {
            return;
        };
        let Some(item) = self.processes.get(process_name).cloned() else {
            return;
        };
        stack.push(process_name.to_owned());
        let run_export_replacements =
            self.instantiate_run_exports(&summary, configured_bindings, substitutions);
        for (stream, cardinality) in &summary.stream_cardinalities {
            let cardinality =
                instantiate_expression(cardinality, substitutions, &run_export_replacements);
            let exact = self
                .constant_quantity(&cardinality)
                .filter(|(value, dimension)| dimension.is_scalar() && value.is_integer());
            self.report.stream_cardinalities.push(StreamCardinality {
                path: object_path.to_owned(),
                stream: stream.clone(),
                cardinality: exact.as_ref().map_or_else(
                    || cardinality.display(),
                    |(value, _)| format_exact_rational(value),
                ),
                status: if exact.is_some() {
                    "exact".into()
                } else {
                    "symbolic".into()
                },
                span: self.span(&item.file_key, cardinality.span.clone()),
            });
        }
        for (stream, timing) in &summary.stream_first_emissions {
            let after =
                instantiate_expression(&timing.after, substitutions, &run_export_replacements);
            let first_item_after = self.metric_value_display(&after, &self.time_type(), None);
            let first_item_after_seconds =
                self.constant_quantity(&after)
                    .and_then(|(value, dimension)| {
                        self.registry
                            .units
                            .get("Time")
                            .is_some_and(|time| time.dimension == dimension)
                            .then(|| format_exact_rational(&value))
                    });
            self.report.stream_handoffs.push(StreamHandoff {
                path: object_path.to_owned(),
                stream: stream.clone(),
                first_item_after,
                first_item_after_seconds,
                status: if self.constant_quantity(&after).is_some() {
                    "exact".into()
                } else {
                    "symbolic".into()
                },
                span: self.span(&item.file_key, timing.span.clone()),
            });
        }

        let inputs = run_inputs(&summary);
        for (run_name, run) in &summary.runs {
            let run = resolve_run_template(run, configured_bindings);
            let mut child_substitutions = IndexMap::new();
            for (name, value) in run.config.iter().chain(&run.arguments) {
                child_substitutions.insert(
                    name.clone(),
                    instantiate_expression(value, substitutions, &run_export_replacements),
                );
            }
            if let Some(child_members) = self.resolved_members.get(&run.process) {
                for (port, member) in child_members {
                    if matches!(member.role, MemberRole::Gets | MemberRole::Uses)
                        && let Some(source) = inputs.get(&(run_name.clone(), port.clone()))
                    {
                        child_substitutions.insert(
                            port.clone(),
                            instantiate_expression(source, substitutions, &run_export_replacements),
                        );
                    }
                }
            }
            self.collect_stream_handoffs(
                &run.process,
                &format!("{object_path}.{run_name}"),
                &child_substitutions,
                &run.configured_arguments,
                stack,
            );
        }
        stack.pop();
    }

    #[allow(clippy::too_many_arguments)]
    fn instantiate_child_obligations(
        &mut self,
        process_name: &str,
        object_path: &str,
        substitutions: &IndexMap<String, Expr>,
        configured_bindings: &IndexMap<String, ConfiguredTemplate>,
        inherited_monitors: &IndexMap<String, MonitorPresentation>,
        instance_multiplicity: &Expr,
        stack: &mut Vec<String>,
    ) {
        if stack.iter().any(|name| name == process_name) {
            return;
        }
        let Some(summary) = self.summaries.get(process_name).cloned() else {
            return;
        };
        let Some(parent_item) = self.processes.get(process_name).cloned() else {
            return;
        };
        let Item::Process(process) = &parent_item.item else {
            return;
        };
        let mut monitored = inherited_monitors.clone();
        let declared_monitors = self
            .resolved_monitors
            .get(process_name)
            .cloned()
            .unwrap_or_default();
        for (metric, display_unit) in &declared_monitors {
            // The outer monitoring context controls a descendant's presentation.
            // A local clause is the default only when no caller is already
            // observing the same metric.
            if !monitored.contains_key(metric) {
                monitored.insert(metric.clone(), display_unit.clone());
            }
        }
        stack.push(process_name.to_owned());
        let run_export_replacements =
            self.instantiate_run_exports(&summary, configured_bindings, substitutions);
        for (metric, presentation) in &monitored {
            let display_unit = &presentation.display_unit;
            let Some(value) = summary.export_values.get(metric) else {
                continue;
            };
            let Some(member) = self
                .resolved_members
                .get(process_name)
                .and_then(|members| members.get(metric))
            else {
                continue;
            };
            let value = instantiate_expression(value, substitutions, &run_export_replacements);
            let value = if member.aggregate == Some(AggregateRule::Sum)
                && !is_one_literal(instance_multiplicity)
            {
                binary_expression(
                    BinaryOp::Mul,
                    value,
                    instance_multiplicity.clone(),
                    process.span.clone(),
                )
            } else {
                value
            };
            let displayed_value =
                self.metric_value_display(&value, &member.ty, display_unit.as_deref());
            let bounds_reason = if self.process_graph_has_stream_edge(
                process_name,
                configured_bindings,
                &mut BTreeSet::new(),
            ) {
                "unresolved pipeline makespan"
            } else {
                "unresolved scheduling makespan"
            };
            let bounds = self.metric_bounds_display(
                &value,
                &member.ty,
                display_unit.as_deref(),
                bounds_reason,
            );
            let expression = value.display();
            let expression = (expression != displayed_value).then_some(expression);
            let (direct_value, direct_expression) = summary
                .direct_export_values
                .get(metric)
                .map(|direct| {
                    let direct =
                        instantiate_expression(direct, substitutions, &run_export_replacements);
                    let direct = if member.aggregate == Some(AggregateRule::Sum)
                        && !is_one_literal(instance_multiplicity)
                    {
                        binary_expression(
                            BinaryOp::Mul,
                            direct,
                            instance_multiplicity.clone(),
                            process.span.clone(),
                        )
                    } else {
                        direct
                    };
                    let displayed =
                        self.metric_value_display(&direct, &member.ty, display_unit.as_deref());
                    let expression = direct.display();
                    let expression = (expression != displayed).then_some(expression);
                    (Some(displayed), expression)
                })
                .unwrap_or((None, None));
            let dependencies = if self.constant_quantity(&value).is_some() {
                Vec::new()
            } else {
                expression_value_dependencies(&value)
            };
            let path = format!("{object_path}.{metric}");
            if !self
                .report
                .monitored_metrics
                .iter()
                .any(|existing| existing.path == path)
            {
                self.report.monitored_metrics.push(MonitoredMetric {
                    path,
                    ty: member.ty.to_string(),
                    approximate_value: (presentation.approximate
                        || expression_requests_approximation(&value))
                    .then(|| crate::presentation::approximate_value(&displayed_value))
                    .flatten(),
                    value: displayed_value,
                    direct_value,
                    direct_expression,
                    display_unit: display_unit.clone(),
                    expression,
                    bounds,
                    aggregate: member
                        .aggregate
                        .map_or_else(|| "explicit".into(), |rule| rule.as_str().into()),
                    span: self.span(&parent_item.file_key, process.span.clone()),
                    dependencies,
                });
            }
        }
        for gap in &summary.metric_coverage_gaps {
            let path = format!("{object_path}.{}.{}", gap.run, gap.metric);
            if monitored.contains_key(&gap.metric) {
                self.push_obligation(
                    &parent_item.file_key,
                    gap.span.clone(),
                    "monitored_metric_coverage",
                    path,
                    gap.expected.to_string(),
                    format!(
                        "monitored metric {} is not exposed by child process {}",
                        gap.metric, gap.process
                    ),
                    None,
                );
            } else {
                let span = self.span(&parent_item.file_key, gap.span.clone());
                self.report.assumed_zero.push(AssumedZero {
                    path: format!("{object_path}.{}", gap.run),
                    contract: format!("{} aggregate {}", gap.metric, gap.rule.as_str()),
                    reason: format!("child {} does not expose {}", gap.process, gap.metric),
                    span: span.clone(),
                });
                self.report.diagnostics.push(Diagnostic::warning(
                    format!("aggregate:{path}"),
                    "W0701",
                    format!(
                        "{path} contributes zero because {} does not expose the aggregate metric {}",
                        gap.process, gap.metric
                    ),
                    span,
                ));
            }
        }
        let inputs = run_inputs(&summary);
        let run_multiplicities = effect_run_multiplicities(&summary, &process.span);
        for (run_name, run) in &summary.runs {
            let run = resolve_run_template(run, configured_bindings);
            let mut child_substitutions = IndexMap::new();
            for (name, value) in run.config.iter().chain(&run.arguments) {
                child_substitutions.insert(
                    name.clone(),
                    instantiate_expression(value, substitutions, &run_export_replacements),
                );
            }
            if let Some(child_members) = self.resolved_members.get(&run.process) {
                for (port, member) in child_members {
                    if matches!(member.role, MemberRole::Gets | MemberRole::Uses)
                        && let Some(source) = inputs.get(&(run_name.clone(), port.clone()))
                    {
                        child_substitutions.insert(
                            port.clone(),
                            instantiate_expression(source, substitutions, &run_export_replacements),
                        );
                    }
                }
            }
            let mut child_monitors = IndexMap::new();
            for (metric, display_unit) in &monitored {
                let Some(parent_member) = self
                    .resolved_members
                    .get(process_name)
                    .and_then(|members| members.get(metric))
                    .cloned()
                else {
                    continue;
                };
                let child_has_metric = self
                    .resolved_members
                    .get(&run.process)
                    .and_then(|members| members.get(metric))
                    .is_some_and(|child| {
                        child.role == MemberRole::Export && compatible(&parent_member.ty, &child.ty)
                    });
                if child_has_metric {
                    child_monitors.insert(metric.clone(), display_unit.clone());
                } else {
                    self.push_obligation(
                        &parent_item.file_key,
                        run.span.clone(),
                        "monitored_metric_coverage",
                        format!("{object_path}.{run_name}.{metric}"),
                        parent_member.ty.to_string(),
                        format!(
                            "monitored metric {metric} is not exposed by child process {}",
                            run.process
                        ),
                        None,
                    );
                }
            }
            if self
                .model
                .processes
                .get(&run.process)
                .is_none_or(|process| process.bodyless)
            {
                self.instantiate_open_process_obligations(
                    &parent_item,
                    object_path,
                    run_name,
                    &run,
                );
            }
            self.bubble_child_obligations(
                &parent_item,
                object_path,
                run_name,
                &run.process,
                &run.span,
            );
            let local_multiplicity = run_multiplicities
                .get(run_name)
                .map(|value| instantiate_expression(value, substitutions, &run_export_replacements))
                .unwrap_or_else(|| integer_expression(1, run.span.clone()));
            let child_multiplicity = if is_one_literal(instance_multiplicity) {
                local_multiplicity
            } else if is_one_literal(&local_multiplicity) {
                instance_multiplicity.clone()
            } else {
                binary_expression(
                    BinaryOp::Mul,
                    instance_multiplicity.clone(),
                    local_multiplicity,
                    run.span.clone(),
                )
            };
            self.instantiate_child_obligations(
                &run.process,
                &format!("{object_path}.{run_name}"),
                &child_substitutions,
                &run.configured_arguments,
                &child_monitors,
                &child_multiplicity,
                stack,
            );
        }
        stack.pop();
    }

    /// Project monitored time makespans into an explicitly conservative report
    /// layer.  Aggregate expressions deliberately omit children that cannot
    /// supply the monitored export so later passes can keep analyzing an
    /// incomplete model.  That is useful for constraints, but it must never
    /// make a known-work range look like a bound for successful completion.
    fn project_temporal_metrics(&mut self) {
        let Some(time_dimension) = self
            .registry
            .units
            .get("Time")
            .map(|unit| unit.dimension.clone())
        else {
            return;
        };

        let metrics = self.report.monitored_metrics.clone();
        self.report.temporal_metrics = metrics
            .into_iter()
            .filter(|metric| {
                metric.aggregate == "makespan"
                    && self
                        .registry
                        .units
                        .get(&metric.ty)
                        .is_some_and(|unit| unit.dimension == time_dimension)
            })
            .map(|metric| {
                let (lower, upper) = metric
                    .bounds
                    .as_ref()
                    .map(|bounds| (bounds.lower.clone(), bounds.upper.clone()))
                    .unwrap_or_else(|| (metric.value.clone(), metric.value.clone()));
                let obligations = self.temporal_completion_obligations(&metric.path);
                let (status, reason, completion_lower, completion_upper) =
                    if !obligations.is_empty() {
                        (
                            "incomplete".to_owned(),
                            Some("open duration obligations".to_owned()),
                            None,
                            None,
                        )
                    } else if let Some(bounds) = &metric.bounds {
                        (
                            "bounded".to_owned(),
                            Some(bounds.reason.clone()),
                            Some(lower.clone()),
                            Some(upper.clone()),
                        )
                    } else if metric.dependencies.is_empty() {
                        (
                            "exact".to_owned(),
                            None,
                            Some(lower.clone()),
                            Some(upper.clone()),
                        )
                    } else {
                        ("symbolic".to_owned(), None, None, None)
                    };
                let lower_seconds = self.displayed_time_seconds(&lower);
                let upper_seconds = self.displayed_time_seconds(&upper);
                TemporalMetric {
                    path: metric.path,
                    display_unit: metric.display_unit,
                    known_work: TemporalRange {
                        lower,
                        upper,
                        lower_seconds,
                        upper_seconds,
                    },
                    completion_lower,
                    completion_upper,
                    status,
                    reason,
                    obligations,
                }
            })
            .collect();
    }

    fn project_resource_schedules(&mut self) {
        self.report.resource_schedules.clear();
        let mut invoked = BTreeSet::new();
        for summary in self.summaries.values() {
            for run in summary.runs.values() {
                invoked.insert(run.process.clone());
                collect_configured_template_processes(&run.configured_arguments, &mut invoked);
            }
        }
        let mut roots = self
            .summaries
            .keys()
            .filter(|name| {
                !invoked.contains(*name)
                    && self
                        .processes
                        .get(*name)
                        .is_some_and(|item| item.file_key == self.sources.entry_key)
            })
            .cloned()
            .collect::<Vec<_>>();
        if roots.is_empty() {
            roots.extend(self.summaries.keys().filter_map(|name| {
                self.processes
                    .get(name)
                    .filter(|item| item.file_key == self.sources.entry_key)
                    .map(|_| name.clone())
            }));
        }
        for root in roots {
            let configured = self.effective_root_configuration(&root);
            self.collect_resource_schedules(
                &root,
                &root,
                &configured.config,
                &configured.configured_arguments,
                &mut Vec::new(),
            );
        }
        self.report
            .resource_schedules
            .sort_by(|left, right| left.path.cmp(&right.path));
        self.reconcile_shared_resource_schedules();
        self.integrate_resource_schedule_bounds();
    }

    /// Reconcile schedules projected from different composites against the
    /// original provisioned pool identity. Each individual schedule is already
    /// feasible; this deterministic pass prevents two otherwise-independent
    /// schedules from both assuming the same residual capacity. It preserves
    /// overlap when their conservative peak claims fit and delays the later
    /// schedule as a block when they do not.
    fn reconcile_shared_resource_schedules(&mut self) {
        #[derive(Clone)]
        struct Window {
            start: BigRational,
            finish: BigRational,
            peaks: HashMap<String, (BigRational, BigRational)>,
        }

        let mut prior = Vec::<Window>::new();
        for index in 0..self.report.resource_schedules.len() {
            let schedule = &self.report.resource_schedules[index];
            let Some(mut start) = schedule
                .stages
                .iter()
                .filter_map(|stage| parse_exact_rational_text(&stage.earliest_start_seconds))
                .min()
            else {
                continue;
            };
            let Some(mut finish) = parse_exact_rational_text(&schedule.makespan_seconds) else {
                continue;
            };
            let mut peaks = HashMap::<String, (BigRational, BigRational)>::new();
            for resource in &schedule.resources {
                let (Some(peak), Some(capacity)) = (
                    parse_exact_rational_text(&resource.peak_claim),
                    parse_exact_rational_text(&resource.capacity),
                ) else {
                    continue;
                };
                peaks
                    .entry(resource.path.clone())
                    .and_modify(|current| {
                        current.0 = (&current.0 + &peak).min(capacity.clone());
                        current.1 = current.1.clone().max(capacity.clone());
                    })
                    .or_insert((peak, capacity));
            }
            if peaks.is_empty() {
                continue;
            }

            let original_start = start.clone();
            let mut shift = BigRational::zero();
            loop {
                let shifted_start = &original_start + &shift;
                let shifted_finish = &finish + &shift;
                let mut required_shift = shift.clone();
                for window in &prior {
                    if shifted_finish <= window.start || shifted_start >= window.finish {
                        continue;
                    }
                    let conflicts = peaks.iter().any(|(path, (peak, capacity))| {
                        window
                            .peaks
                            .get(path)
                            .is_some_and(|(other, other_capacity)| {
                                let capacity = capacity.clone().min(other_capacity.clone());
                                peak + other > capacity
                            })
                    });
                    if conflicts {
                        required_shift = required_shift.max(&window.finish - &original_start);
                    }
                }
                if required_shift == shift {
                    break;
                }
                shift = required_shift;
            }

            if !shift.is_zero() {
                start += &shift;
                finish += &shift;
                let display = self.format_schedule_seconds(&finish);
                let schedule = &mut self.report.resource_schedules[index];
                schedule.makespan_seconds = format_exact_rational(&finish);
                schedule.makespan = display;
                for stage in &mut schedule.stages {
                    if let Some(value) = parse_exact_rational_text(&stage.earliest_ready_seconds) {
                        stage.earliest_ready_seconds = format_exact_rational(&(value + &shift));
                    }
                    if let Some(value) = parse_exact_rational_text(&stage.earliest_start_seconds) {
                        stage.earliest_start_seconds = format_exact_rational(&(value + &shift));
                    }
                    if let Some(value) = parse_exact_rational_text(&stage.latest_finish_seconds) {
                        stage.latest_finish_seconds = format_exact_rational(&(value + &shift));
                    }
                    for bucket in &mut stage.activity {
                        if let Some(value) = parse_exact_rational_text(&bucket.start_seconds) {
                            bucket.start_seconds = format_exact_rational(&(value + &shift));
                        }
                        if let Some(value) = parse_exact_rational_text(&bucket.end_seconds) {
                            bucket.end_seconds = format_exact_rational(&(value + &shift));
                        }
                    }
                }
            }
            prior.push(Window {
                start,
                finish,
                peaks,
            });
        }
    }

    fn integrate_resource_schedule_bounds(&mut self) {
        let schedules = self
            .report
            .resource_schedules
            .iter()
            .map(|schedule| {
                (
                    schedule.path.clone(),
                    schedule.makespan_seconds.clone(),
                    schedule.start_offset_seconds.clone(),
                )
            })
            .collect::<Vec<_>>();
        for (schedule_path, makespan_seconds, start_offset_seconds) in schedules {
            let Some((process_path, _)) = schedule_path.rsplit_once(".schedule[") else {
                continue;
            };
            let duration_path = format!("{process_path}.duration");
            let (Some(absolute_completion), Some(start_offset)) = (
                parse_exact_rational_text(&makespan_seconds),
                parse_exact_rational_text(&start_offset_seconds),
            ) else {
                continue;
            };
            let schedule_seconds = (absolute_completion - start_offset).max(BigRational::zero());
            let existing = self
                .report
                .temporal_metrics
                .iter()
                .find(|metric| metric.path == duration_path)
                .cloned();
            let existing_lower = existing
                .as_ref()
                .and_then(|metric| metric.known_work.lower_seconds.as_deref())
                .and_then(parse_exact_rational_text)
                .unwrap_or_else(BigRational::zero);
            let existing_upper = existing
                .as_ref()
                .and_then(|metric| metric.known_work.upper_seconds.as_deref())
                .and_then(parse_exact_rational_text)
                .unwrap_or_else(BigRational::zero);
            let ancestors = self
                .report
                .temporal_metrics
                .iter()
                .filter_map(|metric| {
                    let instance = metric.path.strip_suffix(".duration")?;
                    (instance != process_path
                        && process_path
                            .strip_prefix(instance)
                            .is_some_and(|suffix| suffix.starts_with('.')))
                    .then(|| {
                        Some((
                            metric.path.clone(),
                            metric
                                .known_work
                                .lower_seconds
                                .as_deref()
                                .and_then(parse_exact_rational_text)?,
                            metric
                                .known_work
                                .upper_seconds
                                .as_deref()
                                .and_then(parse_exact_rational_text)?,
                            metric.display_unit.clone(),
                        ))
                    })?
                })
                .collect::<Vec<_>>();
            let lower = schedule_seconds.clone().max(existing_lower.clone());
            // The resource scheduler and the ordinary causal graph describe
            // the same run interval from different constraints. Under the v1
            // fluid-stream model the completion time is their maximum, not
            // their sum.
            let upper = schedule_seconds.clone().max(existing_upper.clone());
            let display_unit = existing
                .as_ref()
                .and_then(|metric| metric.display_unit.as_deref())
                .filter(|unit| self.registry.units.contains_key(*unit))
                .unwrap_or_else(|| self.schedule_calendar_unit());
            let unit = &self.registry.units[display_unit];
            let lower_display = format!(
                "{} {display_unit}",
                format_exact_rational(&unit.from_canonical(&lower))
            );
            let upper_display = format!(
                "{} {display_unit}",
                format_exact_rational(&unit.from_canonical(&upper))
            );
            if let Some(metric) = self
                .report
                .monitored_metrics
                .iter_mut()
                .find(|metric| metric.path == duration_path)
            {
                metric.bounds = Some(MetricBounds {
                    lower: lower_display.clone(),
                    upper: upper_display.clone(),
                    reason: "finite-resource fluid-stream schedule".into(),
                });
            }
            if let Some(temporal) = self
                .report
                .temporal_metrics
                .iter_mut()
                .find(|metric| metric.path == duration_path)
            {
                temporal.known_work = TemporalRange {
                    lower: lower_display.clone(),
                    upper: upper_display.clone(),
                    lower_seconds: Some(format_exact_rational(&lower)),
                    upper_seconds: Some(format_exact_rational(&upper)),
                };
                temporal.completion_lower = Some(lower_display);
                temporal.completion_upper = Some(upper_display);
                temporal.status = if lower == upper {
                    "exact".into()
                } else {
                    "bounded".into()
                };
                temporal.reason = Some("finite-resource fluid-stream schedule".into());
            }

            // Carry the corrected descendant completion through containing
            // causal schedules. The pre-schedule ancestor/descendant delta is
            // the already-known upstream offset (for example facility
            // provisioning before a batch can borrow its pools).
            for (ancestor_path, ancestor_lower, ancestor_upper, ancestor_unit) in ancestors {
                let offset_lower =
                    (ancestor_lower.clone() - &existing_lower).max(BigRational::zero());
                let offset_upper =
                    (ancestor_upper.clone() - &existing_upper).max(BigRational::zero());
                let propagated_lower = ancestor_lower.max(&schedule_seconds + offset_lower);
                let propagated_upper = ancestor_upper
                    .max(&schedule_seconds + offset_upper)
                    .max(propagated_lower.clone());
                let display_unit = ancestor_unit
                    .as_deref()
                    .filter(|unit| self.registry.units.contains_key(*unit))
                    .unwrap_or(display_unit);
                let unit = &self.registry.units[display_unit];
                let propagated_lower_display = format!(
                    "{} {display_unit}",
                    format_exact_rational(&unit.from_canonical(&propagated_lower))
                );
                let propagated_upper_display = format!(
                    "{} {display_unit}",
                    format_exact_rational(&unit.from_canonical(&propagated_upper))
                );
                if let Some(metric) = self
                    .report
                    .monitored_metrics
                    .iter_mut()
                    .find(|metric| metric.path == ancestor_path)
                {
                    metric.bounds = Some(MetricBounds {
                        lower: propagated_lower_display.clone(),
                        upper: propagated_upper_display.clone(),
                        reason: "finite-resource fluid-stream schedule".into(),
                    });
                }
                if let Some(temporal) = self
                    .report
                    .temporal_metrics
                    .iter_mut()
                    .find(|metric| metric.path == ancestor_path)
                {
                    temporal.known_work = TemporalRange {
                        lower: propagated_lower_display.clone(),
                        upper: propagated_upper_display.clone(),
                        lower_seconds: Some(format_exact_rational(&propagated_lower)),
                        upper_seconds: Some(format_exact_rational(&propagated_upper)),
                    };
                    temporal.completion_lower = Some(propagated_lower_display);
                    temporal.completion_upper = Some(propagated_upper_display);
                    temporal.status = if propagated_lower == propagated_upper {
                        "exact".into()
                    } else {
                        "bounded".into()
                    };
                    temporal.reason = Some("finite-resource fluid-stream schedule".into());
                }
            }
        }
    }

    fn attach_provisioned_resource_utilization(&mut self) {
        let mut utilization =
            HashMap::<String, (BigRational, BigRational, BigRational, BigRational)>::new();
        for schedule in &self.report.resource_schedules {
            let (Some(start), Some(finish)) = (
                parse_exact_rational_text(&schedule.start_offset_seconds),
                parse_exact_rational_text(&schedule.makespan_seconds),
            ) else {
                continue;
            };
            for resource in &schedule.resources {
                let (Some(busy), Some(capacity)) = (
                    parse_exact_rational_text(&resource.busy_capacity_seconds),
                    parse_exact_rational_text(&resource.capacity),
                ) else {
                    continue;
                };
                let available = self
                    .report
                    .provisioned_resources
                    .iter()
                    .find(|pool| pool.path == resource.path)
                    .and_then(|pool| pool.available_after_seconds.as_deref())
                    .and_then(parse_exact_rational_text)
                    .unwrap_or_else(|| start.clone());
                utilization
                    .entry(resource.path.clone())
                    .and_modify(|current| {
                        current.0 += &busy;
                        current.1 = current.1.clone().max(capacity.clone());
                        current.2 = current.2.clone().min(available.clone());
                        current.3 = current.3.clone().max(finish.clone());
                    })
                    .or_insert((busy, capacity, available, finish.clone()));
            }
        }
        for resource in &mut self.report.provisioned_resources {
            resource.utilization =
                utilization
                    .get(&resource.path)
                    .map(|(busy, capacity, start, finish)| {
                        let window = (finish - start).max(BigRational::zero());
                        if capacity.is_zero() || window.is_zero() {
                            "0".to_owned()
                        } else {
                            format_exact_rational(&(busy / (capacity * window)))
                        }
                    });
        }
    }

    fn resource_schedule_start_offset(&self, object_path: &str) -> Option<BigRational> {
        let prefix = format!("{object_path}.");
        let mut result = BigRational::zero();
        for (_, (_, _, _, available_at)) in
            self.resource_pool_bindings.iter().filter(|(endpoint, _)| {
                endpoint
                    .strip_prefix(&prefix)
                    .is_some_and(|suffix| !suffix.contains('.'))
            })
        {
            result = result.max(available_at.clone()?);
        }
        Some(result)
    }

    fn collect_resource_schedules(
        &mut self,
        process_name: &str,
        object_path: &str,
        substitutions: &IndexMap<String, Expr>,
        configured_bindings: &IndexMap<String, ConfiguredTemplate>,
        stack: &mut Vec<String>,
    ) {
        if stack.iter().any(|name| name == process_name) {
            return;
        }
        let Some(summary) = self.summaries.get(process_name).cloned() else {
            return;
        };
        let Some(item) = self.processes.get(process_name).cloned() else {
            return;
        };
        stack.push(process_name.to_owned());
        let mut run_exports =
            self.instantiate_run_exports(&summary, configured_bindings, substitutions);
        for (stream, cardinality) in &summary.stream_cardinalities {
            run_exports.insert(
                format!("cardinality({stream})"),
                instantiate_expression(cardinality, substitutions, &run_exports),
            );
        }

        for (index, schedule) in summary.schedules.iter().enumerate() {
            let jobs = instantiate_expression(&schedule.jobs, substitutions, &run_exports);
            let repetitions =
                instantiate_expression(&schedule.repetitions, substitutions, &run_exports);
            let (Some(jobs), Some(repetitions)) = (
                self.exact_positive_count(&jobs),
                self.exact_positive_count(&repetitions),
            ) else {
                self.push_error(
                    &item.file_key,
                    schedule.span.clone(),
                    "E0720",
                    "resource schedule job and repeat counts must resolve to positive compile-time integers",
                );
                continue;
            };
            let Some(stage_order) = schedule_stage_order(&schedule.stages, &schedule.ordering)
            else {
                self.push_error(
                    &item.file_key,
                    schedule.span.clone(),
                    "E0721",
                    "repeat scheduling block contains a causal cycle",
                );
                continue;
            };
            let operation_count = jobs
                .checked_mul(repetitions)
                .and_then(|count| count.checked_mul(stage_order.len() as u64));
            let Some(operation_count) = operation_count.filter(|count| *count <= 1_000_000) else {
                self.push_error(
                    &item.file_key,
                    schedule.span.clone(),
                    "E0722",
                    "resource schedule exceeds the v1 limit of 1,000,000 operations",
                );
                continue;
            };

            let inputs = run_inputs(&summary);
            let mut stages = Vec::<ConcreteScheduleStage>::new();
            let mut invalid = false;
            let mut resource_owners = HashMap::<String, String>::new();
            for stage_name in &stage_order {
                let Some(duration_expression) = run_exports.get(&format!("{stage_name}.duration"))
                else {
                    self.push_error(
                        &item.file_key,
                        schedule.span.clone(),
                        "E0723",
                        format!("scheduled run {stage_name} does not define duration"),
                    );
                    invalid = true;
                    continue;
                };
                let Some((duration, dimension)) = self.constant_quantity(duration_expression)
                else {
                    self.push_error(
                        &item.file_key,
                        schedule.span.clone(),
                        "E0723",
                        format!("scheduled run {stage_name} duration is not statically known"),
                    );
                    invalid = true;
                    continue;
                };
                if duration <= BigRational::zero()
                    || self
                        .registry
                        .units
                        .get("Second")
                        .is_none_or(|unit| unit.dimension != dimension)
                {
                    self.push_error(
                        &item.file_key,
                        schedule.span.clone(),
                        "E0723",
                        format!("scheduled run {stage_name} requires a positive Time duration"),
                    );
                    invalid = true;
                    continue;
                }

                let Some(run) = summary.runs.get(stage_name) else {
                    invalid = true;
                    continue;
                };
                let run = resolve_run_template(run, configured_bindings);
                let child_claim_substitutions = run
                    .config
                    .iter()
                    .chain(&run.arguments)
                    .map(|(name, value)| {
                        (
                            name.clone(),
                            instantiate_expression(value, substitutions, &run_exports),
                        )
                    })
                    .collect::<IndexMap<_, _>>();
                let child_members = self
                    .resolved_members
                    .get(&run.process)
                    .cloned()
                    .unwrap_or_default();
                let mut resources = Vec::new();
                let mut parallel_capacity = jobs.max(1);
                for (port, member) in child_members {
                    let Type::Resource { .. } = &member.ty else {
                        continue;
                    };
                    if member.role != MemberRole::Uses {
                        continue;
                    }
                    let Some(source) = inputs.get(&(stage_name.clone(), port.clone())) else {
                        continue;
                    };
                    let Some(source_name) = source.as_name() else {
                        self.push_error(
                            &item.file_key,
                            schedule.span.clone(),
                            "E0724",
                            format!("scheduled resource for {stage_name}.{port} must come from a named parent uses port"),
                        );
                        invalid = true;
                        continue;
                    };
                    let Some(source_member) = self
                        .resolved_members
                        .get(process_name)
                        .and_then(|members| members.get(source_name))
                    else {
                        invalid = true;
                        continue;
                    };
                    let endpoint_path = format!("{object_path}.{source_name}");
                    let Some((resource_path, capacity)) = self.instantiated_resource_pool(
                        &endpoint_path,
                        &source_member.ty,
                        substitutions,
                    ) else {
                        self.push_error(
                            &item.file_key,
                            schedule.span.clone(),
                            "E0725",
                            format!("resource capacity {source_name} is not statically known"),
                        );
                        invalid = true;
                        continue;
                    };
                    let Type::Resource {
                        name: source_resource_name,
                        ..
                    } = &source_member.ty
                    else {
                        invalid = true;
                        continue;
                    };
                    if let Some((declared, declared_dimension)) =
                        self.declared_resource_amount(source_resource_name)
                        && (capacity.1 != declared_dimension || capacity.0 > declared)
                    {
                        self.push_error(
                            &item.file_key,
                            schedule.span.clone(),
                            "E0727",
                            format!(
                                "resource {source_name} exceeds {source_resource_name} capacity {}",
                                format_exact_rational(&declared)
                            ),
                        );
                        invalid = true;
                        continue;
                    }
                    let Some(claim) = self.resource_amount(&member.ty, &child_claim_substitutions)
                    else {
                        self.push_error(
                            &item.file_key,
                            schedule.span.clone(),
                            "E0725",
                            format!("resource claim {stage_name}.{port} is not statically known"),
                        );
                        invalid = true;
                        continue;
                    };
                    if capacity.1 != claim.1 || claim.0 <= BigRational::zero() {
                        self.push_error(
                            &item.file_key,
                            schedule.span.clone(),
                            "E0726",
                            format!("resource claim {stage_name}.{port} is incompatible with {source_name}"),
                        );
                        invalid = true;
                        continue;
                    }
                    let slots = (&capacity.0 / &claim.0).to_integer().to_u64().unwrap_or(0);
                    if slots == 0 {
                        self.push_error(
                            &item.file_key,
                            schedule.span.clone(),
                            "E0727",
                            format!("resource {source_name} cannot satisfy one {stage_name}.{port} claim"),
                        );
                        invalid = true;
                        continue;
                    }
                    if let Some(owner) =
                        resource_owners.insert(resource_path.clone(), stage_name.clone())
                        && owner != *stage_name
                    {
                        self.push_error(
                            &item.file_key,
                            schedule.span.clone(),
                            "E0728",
                            format!("resource {source_name} is shared by multiple repeat stages; v1 requires one scheduled stage per resource pool"),
                        );
                        invalid = true;
                    }
                    parallel_capacity = parallel_capacity.min(slots);
                    resources.push(ConcreteScheduleResource {
                        path: resource_path,
                        capacity: capacity.0,
                        claim: claim.0,
                        parallel_capacity: slots,
                    });
                }
                stages.push(ConcreteScheduleStage {
                    name: stage_name.clone(),
                    duration,
                    job_durations: None,
                    duration_display: self.metric_value_display(
                        duration_expression,
                        &self.time_type(),
                        self.authored_calendar_unit("Day"),
                    ),
                    parallel_capacity: parallel_capacity.max(1),
                    resources,
                });
            }
            if invalid || stages.len() != stage_order.len() {
                continue;
            }
            let scheduled = schedule_repeated_jobs(jobs, repetitions, &stages);
            let Some(start_offset) = self.resource_schedule_start_offset(object_path) else {
                continue;
            };
            let activity_profiles =
                schedule_activity_profiles(&scheduled, &start_offset, stages.len());
            let absolute_completion = &start_offset + &scheduled.makespan;
            let makespan_display = self.format_schedule_seconds(&absolute_completion);
            let mut resource_reports = Vec::new();
            for (stage, stage_result) in stages.iter().zip(&scheduled.stages) {
                for resource in &stage.resources {
                    let busy = &stage.duration
                        * BigRational::from_integer(stage_result.operation_count.into())
                        * &resource.claim;
                    let utilization = if scheduled.makespan.is_zero() {
                        BigRational::zero()
                    } else {
                        &busy / (&resource.capacity * &scheduled.makespan)
                    };
                    resource_reports.push(ResourceUtilization {
                        path: resource.path.clone(),
                        stage: format!("{object_path}.{}", stage.name),
                        capacity: format_exact_rational(&resource.capacity),
                        claim_per_operation: format_exact_rational(&resource.claim),
                        parallel_capacity: resource.parallel_capacity,
                        peak_claim: format_exact_rational(
                            &(&resource.claim
                                * BigRational::from_integer(
                                    stage_result
                                        .operation_count
                                        .min(resource.parallel_capacity)
                                        .into(),
                                )),
                        ),
                        busy_capacity_seconds: format_exact_rational(&busy),
                        utilization: format_exact_rational(&utilization),
                    });
                }
            }
            self.report.resource_schedules.push(ResourceSchedule {
                path: format!("{object_path}.schedule[{index}]"),
                jobs,
                repetitions,
                operation_count,
                start_offset_seconds: format_exact_rational(&start_offset),
                makespan: makespan_display,
                makespan_seconds: format_exact_rational(&absolute_completion),
                status: "exact".into(),
                stages: stages
                    .iter()
                    .zip(scheduled.stages)
                    .enumerate()
                    .map(|(stage_index, (stage, result))| ResourceScheduleStage {
                        path: format!("{object_path}.{}", stage.name),
                        operation_count: result.operation_count,
                        duration: stage.duration_display.clone(),
                        duration_seconds: format_exact_rational(&stage.duration),
                        minimum_duration_seconds: format_exact_rational(
                            &schedule_stage_duration_bounds(stage).0,
                        ),
                        maximum_duration_seconds: format_exact_rational(
                            &schedule_stage_duration_bounds(stage).1,
                        ),
                        earliest_ready_seconds: format_exact_rational(
                            &(&start_offset + &result.earliest_ready),
                        ),
                        earliest_start_seconds: format_exact_rational(
                            &(&start_offset + &result.earliest_start),
                        ),
                        latest_finish_seconds: format_exact_rational(
                            &(&start_offset + &result.latest_finish),
                        ),
                        activity: activity_profiles
                            .get(stage_index)
                            .cloned()
                            .unwrap_or_default(),
                        resources: stage
                            .resources
                            .iter()
                            .map(|resource| resource.path.clone())
                            .collect(),
                    })
                    .collect(),
                resources: resource_reports,
                span: self.span(&item.file_key, schedule.span.clone()),
            });
        }

        let mut effect_schedule_index = summary.schedules.len();
        for effect in effect_schedule_roots(&summary.effects) {
            self.collect_effect_resource_schedule(
                process_name,
                object_path,
                &summary,
                &item,
                substitutions,
                configured_bindings,
                &run_exports,
                effect_schedule_index,
                effect,
            );
            effect_schedule_index += 1;
        }
        self.collect_direct_resource_schedule(
            process_name,
            object_path,
            &summary,
            &item,
            substitutions,
            configured_bindings,
            &run_exports,
            effect_schedule_index,
        );

        let inputs = run_inputs(&summary);
        for (run_name, run) in &summary.runs {
            let run = resolve_run_template(run, configured_bindings);
            let mut child_substitutions = IndexMap::new();
            for (name, value) in run.config.iter().chain(&run.arguments) {
                child_substitutions.insert(
                    name.clone(),
                    instantiate_expression(value, substitutions, &run_exports),
                );
            }
            if let Some(child_members) = self.resolved_members.get(&run.process) {
                for (port, member) in child_members {
                    if matches!(member.role, MemberRole::Gets | MemberRole::Uses)
                        && let Some(source) = inputs.get(&(run_name.clone(), port.clone()))
                    {
                        child_substitutions.insert(
                            port.clone(),
                            instantiate_expression(source, substitutions, &run_exports),
                        );
                    }
                }
            }
            self.collect_resource_schedules(
                &run.process,
                &format!("{object_path}.{run_name}"),
                &child_substitutions,
                &run.configured_arguments,
                stack,
            );
        }
        stack.pop();
    }

    /// Schedule direct child runs that borrow capacity but do not live inside
    /// a finite physical loop. Without this projection, fan-out from one pool
    /// to sibling runs would validate each claim independently and could clone
    /// the pool's capacity in time.
    #[allow(clippy::too_many_arguments)]
    fn collect_direct_resource_schedule(
        &mut self,
        process_name: &str,
        object_path: &str,
        summary: &ProcessSummary,
        item: &ItemRef,
        substitutions: &IndexMap<String, Expr>,
        configured_bindings: &IndexMap<String, ConfiguredTemplate>,
        run_exports: &HashMap<String, Expr>,
        index: usize,
    ) {
        let direct_names = summary
            .effects
            .iter()
            .filter_map(|effect| match effect {
                EffectTemplate::Run { name, .. } => Some(name.clone()),
                _ => None,
            })
            .collect::<Vec<_>>();
        if direct_names.is_empty() {
            return;
        }
        let direct_set = direct_names.iter().cloned().collect::<BTreeSet<_>>();
        let mut predecessors = direct_names
            .iter()
            .map(|name| (name.clone(), BTreeSet::<String>::new()))
            .collect::<HashMap<_, _>>();
        let all_instances = direct_names
            .iter()
            .enumerate()
            .map(|(index, name)| (name.clone(), index))
            .collect::<HashMap<_, _>>();
        for name in &direct_names {
            let run = resolve_run_template(summary.runs.get(name).unwrap(), configured_bindings);
            for value in run.config.values().chain(run.arguments.values()) {
                for producer in
                    schedule_expression_producers(value, &all_instances, &HashMap::new())
                {
                    predecessors
                        .get_mut(name)
                        .unwrap()
                        .insert(direct_names[producer].clone());
                }
            }
        }
        for (source, target, _) in &summary.wires {
            let Some(target_path) = target.path() else {
                continue;
            };
            let Some(target_run) = target_path.split('.').next() else {
                continue;
            };
            if !direct_set.contains(target_run) {
                continue;
            }
            for producer in schedule_expression_producers(source, &all_instances, &HashMap::new()) {
                predecessors
                    .get_mut(target_run)
                    .unwrap()
                    .insert(direct_names[producer].clone());
            }
        }
        for (first, second) in &summary.ordering {
            if direct_set.contains(first) && direct_set.contains(second) {
                predecessors.get_mut(second).unwrap().insert(first.clone());
            }
        }

        let resource_runs = direct_names
            .iter()
            .filter(|name| {
                let run =
                    resolve_run_template(summary.runs.get(*name).unwrap(), configured_bindings);
                !self
                    .model
                    .processes
                    .get(&run.process)
                    .is_some_and(|process| process.composite)
                    && self
                        .resolved_members
                        .get(&run.process)
                        .is_some_and(|members| {
                            members.values().any(|member| {
                                member.role == MemberRole::Uses
                                    && matches!(member.ty, Type::Resource { .. })
                            })
                        })
            })
            .cloned()
            .collect::<Vec<_>>();
        if resource_runs.is_empty() {
            return;
        }
        let mut needed = resource_runs.into_iter().collect::<BTreeSet<_>>();
        let mut queue = needed.iter().cloned().collect::<VecDeque<_>>();
        while let Some(run) = queue.pop_front() {
            for predecessor in predecessors.get(&run).into_iter().flatten() {
                if needed.insert(predecessor.clone()) {
                    queue.push_back(predecessor.clone());
                }
            }
        }
        let stage_names = direct_names
            .into_iter()
            .filter(|name| needed.contains(name))
            .collect::<Vec<_>>();
        let stage_indices = stage_names
            .iter()
            .enumerate()
            .map(|(index, name)| (name.clone(), index))
            .collect::<HashMap<_, _>>();
        let inputs = run_inputs(summary);
        let Item::Process(process) = &item.item else {
            return;
        };
        let Some(parent_members) = self.resolved_members.get(process_name) else {
            return;
        };
        let stream_handoffs =
            self.effect_stream_handoffs(summary, substitutions, configured_bindings, run_exports);
        let mut stages = Vec::with_capacity(stage_names.len());
        let mut operations = Vec::with_capacity(stage_names.len());

        for name in &stage_names {
            let Some(duration_expression) = run_exports.get(&format!("{name}.duration")) else {
                return;
            };
            let Some((duration, dimension)) = self.constant_quantity(duration_expression) else {
                return;
            };
            if duration < BigRational::zero()
                || self
                    .registry
                    .units
                    .get("Second")
                    .is_none_or(|unit| unit.dimension != dimension)
            {
                return;
            }
            let run = resolve_run_template(summary.runs.get(name).unwrap(), configured_bindings);
            let child_substitutions = run
                .config
                .iter()
                .chain(&run.arguments)
                .map(|(parameter, value)| {
                    (
                        parameter.clone(),
                        instantiate_expression(value, substitutions, run_exports),
                    )
                })
                .collect::<IndexMap<_, _>>();
            let mut resources = Vec::new();
            let mut parallel_capacity = 1_u64;
            if let Some(child_members) = self.resolved_members.get(&run.process) {
                for (port, member) in child_members {
                    if member.role != MemberRole::Uses
                        || !matches!(member.ty, Type::Resource { .. })
                    {
                        continue;
                    }
                    let Some(source) = inputs.get(&(name.clone(), port.clone())) else {
                        continue;
                    };
                    let source_type =
                        self.endpoint_type(process, summary, parent_members, source, true);
                    let Some(source_path) = instance_endpoint_path(object_path, source) else {
                        return;
                    };
                    let Some((resource_path, capacity)) =
                        self.instantiated_resource_pool(&source_path, &source_type, substitutions)
                    else {
                        return;
                    };
                    let Some(claim) = self.resource_amount(&member.ty, &child_substitutions) else {
                        return;
                    };
                    if capacity.1 != claim.1
                        || claim.0 <= BigRational::zero()
                        || claim.0 > capacity.0
                    {
                        return;
                    }
                    let slots = (&capacity.0 / &claim.0).to_integer().to_u64().unwrap_or(0);
                    parallel_capacity = parallel_capacity.max(slots.max(1));
                    resources.push(ConcreteScheduleResource {
                        path: resource_path,
                        capacity: capacity.0,
                        claim: claim.0,
                        parallel_capacity: slots,
                    });
                }
            }
            stages.push(ConcreteScheduleStage {
                name: name.clone(),
                duration: duration.clone(),
                job_durations: None,
                duration_display: self.metric_value_display(
                    duration_expression,
                    &self.time_type(),
                    self.authored_calendar_unit("Day"),
                ),
                parallel_capacity,
                resources,
            });

            let mut dependencies = predecessors
                .get(name)
                .into_iter()
                .flatten()
                .filter_map(|predecessor| stage_indices.get(predecessor).copied())
                .map(ScheduleDependency::Completion)
                .collect::<Vec<_>>();
            // Preserve first-item overlap for a direct stream wire. The close
            // dependency still extends the consumer's occupied interval.
            for (source, target, _) in &summary.wires {
                if target
                    .path()
                    .as_deref()
                    .and_then(|path| path.split('.').next())
                    != Some(name.as_str())
                {
                    continue;
                }
                let ExprKind::Member { value, member } = &source.kind else {
                    continue;
                };
                let Some(producer_name) = value.as_name() else {
                    continue;
                };
                let Some(producer) = stage_indices.get(producer_name).copied() else {
                    continue;
                };
                let producer_run = resolve_run_template(
                    summary.runs.get(producer_name).unwrap(),
                    configured_bindings,
                );
                let is_stream = self
                    .resolved_members
                    .get(&producer_run.process)
                    .and_then(|members| members.get(member))
                    .is_some_and(|member| matches!(member.ty, Type::Stream(_)));
                if is_stream
                    && let Some(delay) = stream_handoffs
                        .get(&(producer_name.to_owned(), member.clone()))
                        .cloned()
                        .flatten()
                {
                    dependencies.retain(|dependency| {
                        *dependency != ScheduleDependency::Completion(producer)
                    });
                    dependencies.push(ScheduleDependency::Handoff {
                        operation: producer,
                        delay,
                    });
                    dependencies.push(ScheduleDependency::Close(producer));
                }
            }
            dedup_schedule_dependencies(&mut dependencies);
            operations.push(ConcreteScheduleOperation {
                stage: operations.len(),
                dependencies,
            });
        }

        let scheduled = schedule_effect_jobs(1, &operations, &stages);
        if !scheduled.complete {
            return;
        }
        let Some(start_offset) = self.resource_schedule_start_offset(object_path) else {
            return;
        };
        let activity_profiles = schedule_activity_profiles(&scheduled, &start_offset, stages.len());
        let absolute_completion = &start_offset + &scheduled.makespan;
        let mut resources = Vec::new();
        for (stage, stage_result) in stages.iter().zip(&scheduled.stages) {
            for resource in &stage.resources {
                let busy = &stage_result.occupied_duration * &resource.claim;
                let utilization = if scheduled.makespan.is_zero() {
                    BigRational::zero()
                } else {
                    &busy / (&resource.capacity * &scheduled.makespan)
                };
                resources.push(ResourceUtilization {
                    path: resource.path.clone(),
                    stage: format!("{object_path}.{}", stage.name),
                    capacity: format_exact_rational(&resource.capacity),
                    claim_per_operation: format_exact_rational(&resource.claim),
                    parallel_capacity: resource.parallel_capacity,
                    peak_claim: format_exact_rational(
                        &(&resource.claim
                            * BigRational::from_integer(
                                stage_result
                                    .operation_count
                                    .min(resource.parallel_capacity)
                                    .into(),
                            )),
                    ),
                    busy_capacity_seconds: format_exact_rational(&busy),
                    utilization: format_exact_rational(&utilization),
                });
            }
        }
        self.report.resource_schedules.push(ResourceSchedule {
            path: format!("{object_path}.schedule[{index}]"),
            jobs: 1,
            repetitions: 1,
            operation_count: operations.len() as u64,
            start_offset_seconds: format_exact_rational(&start_offset),
            makespan: self.format_schedule_seconds(&absolute_completion),
            makespan_seconds: format_exact_rational(&absolute_completion),
            status: "exact".into(),
            stages: stages
                .iter()
                .zip(scheduled.stages)
                .enumerate()
                .map(|(stage_index, (stage, result))| ResourceScheduleStage {
                    path: format!("{object_path}.{}", stage.name),
                    operation_count: result.operation_count,
                    duration: stage.duration_display.clone(),
                    duration_seconds: format_exact_rational(&stage.duration),
                    minimum_duration_seconds: format_exact_rational(
                        &schedule_stage_duration_bounds(stage).0,
                    ),
                    maximum_duration_seconds: format_exact_rational(
                        &schedule_stage_duration_bounds(stage).1,
                    ),
                    earliest_ready_seconds: format_exact_rational(
                        &(&start_offset + &result.earliest_ready),
                    ),
                    earliest_start_seconds: format_exact_rational(
                        &(&start_offset + &result.earliest_start),
                    ),
                    latest_finish_seconds: format_exact_rational(
                        &(&start_offset + &result.latest_finish),
                    ),
                    activity: activity_profiles
                        .get(stage_index)
                        .cloned()
                        .unwrap_or_default(),
                    resources: stage
                        .resources
                        .iter()
                        .map(|resource| resource.path.clone())
                        .collect(),
                })
                .collect(),
            resources,
            span: self.span(&item.file_key, item.item.span().clone()),
        });
    }

    #[allow(clippy::too_many_arguments)]
    fn collect_effect_resource_schedule(
        &mut self,
        process_name: &str,
        object_path: &str,
        summary: &ProcessSummary,
        item: &ItemRef,
        substitutions: &IndexMap<String, Expr>,
        configured_bindings: &IndexMap<String, ConfiguredTemplate>,
        run_exports: &HashMap<String, Expr>,
        index: usize,
        effect: &EffectTemplate,
    ) {
        let EffectTemplate::For {
            binding,
            iterable,
            body,
            span,
            ..
        } = effect
        else {
            return;
        };
        let job_count_expression = iteration_count_expression(iterable, span)
            .unwrap_or_else(|| cardinality_expression(iterable, span));
        let job_count_expression =
            instantiate_expression(&job_count_expression, substitutions, run_exports);
        let Some(jobs) = self.exact_nonnegative_count(&job_count_expression) else {
            self.push_error(
                &item.file_key,
                span.clone(),
                "E0720",
                format!(
                    "physical loop job count must resolve to a nonnegative compile-time integer; found {}",
                    job_count_expression.display()
                ),
            );
            return;
        };

        let mut symbolic_operations = Vec::new();
        let mut run_instances = HashMap::new();
        let mut value_producers = HashMap::new();
        let mut reported_repetitions = 1_u64;
        let stream_handoffs =
            self.effect_stream_handoffs(summary, substitutions, configured_bindings, run_exports);
        let job_items = self.materialize_iterable(
            process_name,
            summary,
            iterable,
            substitutions,
            substitutions,
            configured_bindings,
            run_exports,
            &HashMap::new(),
            &mut Vec::new(),
        );
        let mut effect_values = substitutions.clone();
        effect_values.insert(binding.clone(), Expr::name(binding, iterable.span.clone()));
        self.collect_effect_value_aliases(body, &mut effect_values, run_exports);
        let local_release = self
            .effect_schedule_release_offset(
                iterable,
                summary,
                configured_bindings,
                run_exports,
                &stream_handoffs,
            )
            .unwrap_or_else(BigRational::zero);
        if !self.build_effect_schedule_operations(
            body,
            substitutions,
            run_exports,
            item,
            summary,
            &mut symbolic_operations,
            &mut run_instances,
            &mut value_producers,
            &BTreeSet::new(),
            &stream_handoffs,
            &mut reported_repetitions,
        ) {
            return;
        }
        if symbolic_operations.is_empty() {
            return;
        }
        let has_capacity_resource = symbolic_operations.iter().any(|operation| {
            summary
                .runs
                .get(&operation.run)
                .map(|run| resolve_run_template(run, configured_bindings))
                .and_then(|run| self.resolved_members.get(&run.process))
                .is_some_and(|members| {
                    members.values().any(|member| {
                        member.role == MemberRole::Uses
                            && matches!(member.ty, Type::Resource { .. })
                    })
                })
        });
        if !has_capacity_resource {
            return;
        }
        let Some(operation_count) = jobs
            .checked_mul(symbolic_operations.len() as u64)
            .filter(|count| *count <= 1_000_000)
        else {
            self.push_error(
                &item.file_key,
                span.clone(),
                "E0722",
                "physical loop schedule exceeds the v1 limit of 1,000,000 operations",
            );
            return;
        };

        let mut stage_names = Vec::<String>::new();
        for operation in &symbolic_operations {
            if !stage_names.contains(&operation.run) {
                stage_names.push(operation.run.clone());
            }
        }
        let inputs = run_inputs(summary);
        let mut stages = Vec::<ConcreteScheduleStage>::new();
        let mut invalid = false;
        for stage_name in &stage_names {
            let Some(duration_expression) =
                run_exports.get(&format!("{stage_name}.duration")).cloned()
            else {
                self.push_error(
                    &item.file_key,
                    span.clone(),
                    "E0723",
                    format!("physical run {stage_name} does not define duration"),
                );
                invalid = true;
                continue;
            };
            let duration_expression =
                instantiate_expression(&duration_expression, substitutions, run_exports);
            let resolved_duration = self.constant_quantity(&duration_expression);
            let per_job_durations = resolved_duration.is_none().then(|| {
                let items = job_items.as_ref()?;
                if items.len() != jobs as usize {
                    return None;
                }
                let mut durations = Vec::with_capacity(items.len());
                let mut duration_dimension = None;
                for item in items {
                    let mut values = effect_values.clone();
                    values.insert(binding.clone(), item.clone());
                    let expression =
                        self.materialize_effect_expression(&duration_expression, &values);
                    let (duration, dimension) = self.constant_quantity(&expression)?;
                    if duration_dimension
                        .as_ref()
                        .is_some_and(|expected| expected != &dimension)
                    {
                        return None;
                    }
                    duration_dimension = Some(dimension);
                    durations.push(duration);
                }
                Some((durations, duration_dimension?))
            });
            let (duration, dimension, job_durations) = match (resolved_duration, per_job_durations)
            {
                (Some((duration, dimension)), _) => (duration, dimension, None),
                (None, Some(Some((durations, dimension)))) => {
                    let total = durations.iter().cloned().sum::<BigRational>();
                    let average = if durations.is_empty() {
                        BigRational::zero()
                    } else {
                        total / BigRational::from_integer(durations.len().into())
                    };
                    (average, dimension, Some(durations))
                }
                _ => {
                    self.push_error(
                        &item.file_key,
                        span.clone(),
                        "E0723",
                        format!(
                            "physical run {stage_name} duration is not statically known: {}",
                            duration_expression.display()
                        ),
                    );
                    invalid = true;
                    continue;
                }
            };
            if duration < BigRational::zero()
                || self
                    .registry
                    .units
                    .get("Second")
                    .is_none_or(|unit| unit.dimension != dimension)
            {
                self.push_error(
                    &item.file_key,
                    span.clone(),
                    "E0723",
                    format!("physical run {stage_name} requires a nonnegative Time duration"),
                );
                invalid = true;
                continue;
            }

            let Some(run) = summary.runs.get(stage_name) else {
                invalid = true;
                continue;
            };
            let run = resolve_run_template(run, configured_bindings);
            let child_claim_substitutions = run
                .config
                .iter()
                .chain(&run.arguments)
                .map(|(name, value)| {
                    (
                        name.clone(),
                        instantiate_expression(value, substitutions, run_exports),
                    )
                })
                .collect::<IndexMap<_, _>>();
            let child_members = self
                .resolved_members
                .get(&run.process)
                .cloned()
                .unwrap_or_default();
            let mut resources = Vec::new();
            let mut parallel_capacity = jobs.max(1);
            for (port, member) in child_members {
                let Type::Resource { .. } = &member.ty else {
                    continue;
                };
                if member.role != MemberRole::Uses {
                    continue;
                }
                let Some(source) = inputs.get(&(stage_name.clone(), port.clone())) else {
                    continue;
                };
                let Some(source_name) = source.as_name() else {
                    self.push_error(
                        &item.file_key,
                        span.clone(),
                        "E0724",
                        format!(
                            "physical resource for {stage_name}.{port} must come from a named parent uses port"
                        ),
                    );
                    invalid = true;
                    continue;
                };
                let Some(source_member) = self
                    .resolved_members
                    .get(process_name)
                    .and_then(|members| members.get(source_name))
                else {
                    invalid = true;
                    continue;
                };
                let endpoint_path = format!("{object_path}.{source_name}");
                let Some((resource_path, capacity)) = self.instantiated_resource_pool(
                    &endpoint_path,
                    &source_member.ty,
                    substitutions,
                ) else {
                    self.push_error(
                        &item.file_key,
                        span.clone(),
                        "E0725",
                        format!("resource capacity {source_name} is not statically known"),
                    );
                    invalid = true;
                    continue;
                };
                let Some(claim) = self.resource_amount(&member.ty, &child_claim_substitutions)
                else {
                    self.push_error(
                        &item.file_key,
                        span.clone(),
                        "E0725",
                        format!("resource claim {stage_name}.{port} is not statically known"),
                    );
                    invalid = true;
                    continue;
                };
                if capacity.1 != claim.1 || claim.0 <= BigRational::zero() {
                    self.push_error(
                        &item.file_key,
                        span.clone(),
                        "E0726",
                        format!(
                            "resource claim {stage_name}.{port} is incompatible with {source_name}"
                        ),
                    );
                    invalid = true;
                    continue;
                }
                let slots = (&capacity.0 / &claim.0).to_integer().to_u64().unwrap_or(0);
                if slots == 0 {
                    self.push_error(
                        &item.file_key,
                        span.clone(),
                        "E0727",
                        format!(
                            "resource {source_name} cannot satisfy one {stage_name}.{port} claim"
                        ),
                    );
                    invalid = true;
                    continue;
                }
                parallel_capacity = parallel_capacity.min(slots);
                resources.push(ConcreteScheduleResource {
                    path: resource_path,
                    capacity: capacity.0,
                    claim: claim.0,
                    parallel_capacity: slots,
                });
            }
            stages.push(ConcreteScheduleStage {
                name: stage_name.clone(),
                duration,
                job_durations,
                duration_display: self.metric_value_display(
                    &duration_expression,
                    &self.time_type(),
                    self.authored_calendar_unit("Day"),
                ),
                parallel_capacity: parallel_capacity.max(1),
                resources,
            });
        }
        if invalid || stages.len() != stage_names.len() {
            return;
        }

        let operations = symbolic_operations
            .iter()
            .filter_map(|operation| {
                Some(ConcreteScheduleOperation {
                    stage: stage_names
                        .iter()
                        .position(|stage| stage == &operation.run)?,
                    dependencies: operation.dependencies.clone(),
                })
            })
            .collect::<Vec<_>>();
        let scheduled = schedule_effect_jobs(jobs, &operations, &stages);
        if !scheduled.complete {
            self.push_error(
                &item.file_key,
                span.clone(),
                "E0721",
                "physical operation graph contains a causal cycle",
            );
            return;
        }
        let Some(start_offset) = self.resource_schedule_start_offset(object_path) else {
            return;
        };
        let activity_offset = &start_offset + &local_release;
        let activity_profiles =
            schedule_activity_profiles(&scheduled, &activity_offset, stages.len());
        let absolute_completion = &start_offset + &local_release + &scheduled.makespan;
        let makespan_display = self.format_schedule_seconds(&absolute_completion);
        let mut resource_reports = Vec::new();
        for (stage, stage_result) in stages.iter().zip(&scheduled.stages) {
            for resource in &stage.resources {
                let busy = &stage_result.occupied_duration * &resource.claim;
                let utilization = if scheduled.makespan.is_zero() {
                    BigRational::zero()
                } else {
                    &busy / (&resource.capacity * &scheduled.makespan)
                };
                resource_reports.push(ResourceUtilization {
                    path: resource.path.clone(),
                    stage: format!("{object_path}.{}", stage.name),
                    capacity: format_exact_rational(&resource.capacity),
                    claim_per_operation: format_exact_rational(&resource.claim),
                    parallel_capacity: resource.parallel_capacity,
                    peak_claim: format_exact_rational(
                        &(&resource.claim
                            * BigRational::from_integer(
                                stage_result
                                    .operation_count
                                    .min(resource.parallel_capacity)
                                    .into(),
                            )),
                    ),
                    busy_capacity_seconds: format_exact_rational(&busy),
                    utilization: format_exact_rational(&utilization),
                });
            }
        }
        self.report.resource_schedules.push(ResourceSchedule {
            path: format!("{object_path}.schedule[{index}]"),
            jobs,
            repetitions: reported_repetitions,
            operation_count,
            start_offset_seconds: format_exact_rational(&start_offset),
            makespan: makespan_display,
            makespan_seconds: format_exact_rational(&absolute_completion),
            status: "exact".into(),
            stages: stages
                .iter()
                .zip(scheduled.stages)
                .enumerate()
                .map(|(stage_index, (stage, result))| ResourceScheduleStage {
                    path: format!("{object_path}.{}", stage.name),
                    operation_count: result.operation_count,
                    duration: stage.duration_display.clone(),
                    duration_seconds: format_exact_rational(&stage.duration),
                    minimum_duration_seconds: format_exact_rational(
                        &schedule_stage_duration_bounds(stage).0,
                    ),
                    maximum_duration_seconds: format_exact_rational(
                        &schedule_stage_duration_bounds(stage).1,
                    ),
                    earliest_ready_seconds: format_exact_rational(
                        &(&activity_offset + &result.earliest_ready),
                    ),
                    earliest_start_seconds: format_exact_rational(
                        &(&start_offset + &local_release + &result.earliest_start),
                    ),
                    latest_finish_seconds: format_exact_rational(
                        &(&start_offset + &local_release + &result.latest_finish),
                    ),
                    activity: activity_profiles
                        .get(stage_index)
                        .cloned()
                        .unwrap_or_default(),
                    resources: stage
                        .resources
                        .iter()
                        .map(|resource| resource.path.clone())
                        .collect(),
                })
                .collect(),
            resources: resource_reports,
            span: self.span(&item.file_key, span.clone()),
        });
    }

    #[allow(clippy::too_many_arguments)]
    fn effect_stream_handoffs(
        &self,
        summary: &ProcessSummary,
        substitutions: &IndexMap<String, Expr>,
        configured_bindings: &IndexMap<String, ConfiguredTemplate>,
        parent_run_exports: &HashMap<String, Expr>,
    ) -> HashMap<(String, String), Option<BigRational>> {
        let mut result = HashMap::new();
        for (run_name, template) in &summary.runs {
            let run = resolve_run_template(template, configured_bindings);
            let Some(child_summary) = self.summaries.get(&run.process) else {
                continue;
            };
            let child_substitutions = run
                .config
                .iter()
                .chain(&run.arguments)
                .map(|(name, value)| {
                    (
                        name.clone(),
                        instantiate_expression(value, substitutions, parent_run_exports),
                    )
                })
                .collect::<IndexMap<_, _>>();
            let child_exports = self.instantiate_run_exports(
                child_summary,
                &run.configured_arguments,
                &child_substitutions,
            );
            for (stream, timing) in &child_summary.stream_first_emissions {
                let cardinality_is_zero = child_summary
                    .stream_cardinalities
                    .get(stream)
                    .map(|cardinality| {
                        instantiate_expression(cardinality, &child_substitutions, &child_exports)
                    })
                    .and_then(|cardinality| self.exact_nonnegative_count(&cardinality))
                    == Some(0);
                let delay = (!cardinality_is_zero)
                    .then(|| {
                        instantiate_expression(&timing.after, &child_substitutions, &child_exports)
                    })
                    .and_then(|delay| self.constant_quantity(&delay))
                    .and_then(|(delay, dimension)| {
                        self.registry
                            .units
                            .get("Second")
                            .is_some_and(|unit| unit.dimension == dimension)
                            .then_some(delay)
                    });
                result.insert((run_name.clone(), stream.clone()), delay);
            }
        }
        result
    }

    fn materialize_effect_expression(
        &self,
        expression: &Expr,
        values: &IndexMap<String, Expr>,
    ) -> Expr {
        let mut expression = expression.clone();
        let mut seen = BTreeSet::new();
        for _ in 0..=values.len() {
            let display = expression.display();
            if !seen.insert(display.clone()) {
                break;
            }
            let next = substitute_names(&expression, values);
            if next.display() == display {
                expression = next;
                break;
            }
            expression = next;
        }
        substitute_paths(&expression, &HashMap::new())
    }

    fn collect_effect_value_aliases(
        &self,
        effects: &[EffectTemplate],
        values: &mut IndexMap<String, Expr>,
        run_exports: &HashMap<String, Expr>,
    ) {
        for effect in effects {
            match effect {
                EffectTemplate::Bind { name, value, .. } => {
                    let value = instantiate_expression(value, values, run_exports);
                    values.insert(
                        name.clone(),
                        self.materialize_effect_expression(&value, values),
                    );
                }
                EffectTemplate::Assign { target, value, .. } => {
                    if let Some(name) = target.as_name() {
                        let value = instantiate_expression(value, values, run_exports);
                        values.insert(
                            name.to_owned(),
                            self.materialize_effect_expression(&value, values),
                        );
                    }
                }
                EffectTemplate::Repeat { count, body, .. } => {
                    let count = self.materialize_effect_expression(count, values);
                    if let Some(count) = self.exact_nonnegative_count(&count) {
                        for _ in 0..count {
                            self.collect_effect_value_aliases(body, values, run_exports);
                        }
                    }
                }
                EffectTemplate::Branch {
                    condition,
                    then_body,
                    else_body,
                    ..
                } => {
                    let condition = self.materialize_effect_expression(condition, values);
                    if let Some(selected) = self.evaluate_predicate(&condition) {
                        self.collect_effect_value_aliases(
                            if selected { then_body } else { else_body },
                            values,
                            run_exports,
                        );
                    }
                }
                EffectTemplate::For { .. }
                | EffectTemplate::Run { .. }
                | EffectTemplate::Wire { .. }
                | EffectTemplate::Yield { .. }
                | EffectTemplate::Before { .. } => {}
            }
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn materialize_stream_source(
        &self,
        parent_summary: &ProcessSummary,
        source: &Expr,
        substitutions: &IndexMap<String, Expr>,
        configured_bindings: &IndexMap<String, ConfiguredTemplate>,
        run_exports: &HashMap<String, Expr>,
        input_streams: &HashMap<String, Vec<Expr>>,
        stack: &mut Vec<(String, String)>,
    ) -> Option<Vec<Expr>> {
        let source = instantiate_expression(source, substitutions, run_exports);
        if let Some(name) = source.as_name() {
            return input_streams.get(name).cloned();
        }
        let ExprKind::Member { value, member } = &source.kind else {
            return None;
        };
        let run_name = value.as_name()?;
        let run = resolve_run_template(parent_summary.runs.get(run_name)?, configured_bindings);
        let child_summary = self.summaries.get(&run.process)?;
        let parent_inputs = run_inputs(parent_summary);
        let mut child_substitutions = run
            .config
            .iter()
            .chain(&run.arguments)
            .map(|(name, value)| {
                (
                    name.clone(),
                    instantiate_expression(value, substitutions, run_exports),
                )
            })
            .collect::<IndexMap<_, _>>();
        let mut child_input_streams = HashMap::new();
        if let Some(child_members) = self.resolved_members.get(&run.process) {
            for (port, child_member) in child_members {
                if !matches!(child_member.role, MemberRole::Gets | MemberRole::Uses) {
                    continue;
                }
                let Some(input) = parent_inputs.get(&(run_name.to_owned(), port.clone())) else {
                    continue;
                };
                if matches!(child_member.ty, Type::Stream(_)) {
                    if let Some(items) = self.materialize_stream_source(
                        parent_summary,
                        input,
                        substitutions,
                        configured_bindings,
                        run_exports,
                        input_streams,
                        stack,
                    ) {
                        child_input_streams.insert(port.clone(), items);
                    }
                } else {
                    child_substitutions.insert(
                        port.clone(),
                        instantiate_expression(input, substitutions, run_exports),
                    );
                }
            }
        }
        let child_exports = self.instantiate_run_exports(
            child_summary,
            &run.configured_arguments,
            &child_substitutions,
        );
        self.materialize_process_stream(
            &run.process,
            child_summary,
            member,
            &child_substitutions,
            &run.configured_arguments,
            &child_exports,
            &child_input_streams,
            stack,
        )
    }

    #[allow(clippy::too_many_arguments)]
    fn materialize_iterable(
        &self,
        process_name: &str,
        summary: &ProcessSummary,
        iterable: &Expr,
        values: &IndexMap<String, Expr>,
        substitutions: &IndexMap<String, Expr>,
        configured_bindings: &IndexMap<String, ConfiguredTemplate>,
        run_exports: &HashMap<String, Expr>,
        input_streams: &HashMap<String, Vec<Expr>>,
        stack: &mut Vec<(String, String)>,
    ) -> Option<Vec<Expr>> {
        let iterable = self.materialize_effect_expression(iterable, values);
        if let ExprKind::Array { items } = &iterable.kind {
            return Some(items.clone());
        }
        if let ExprKind::Call { callee, arguments } = &iterable.kind {
            match callee.path().as_deref() {
                Some("math.range") if arguments.len() == 2 => {
                    let (start_value, start_dimension) =
                        self.constant_quantity(&arguments[0].value)?;
                    let (end_value, end_dimension) = self.constant_quantity(&arguments[1].value)?;
                    if !start_dimension.is_scalar()
                        || !end_dimension.is_scalar()
                        || !start_value.is_integer()
                        || !end_value.is_integer()
                    {
                        return None;
                    }
                    let start = start_value.to_integer().to_i64()?;
                    let end = end_value.to_integer().to_i64()?;
                    return Some(
                        (start..end)
                            .map(|value| integer_expression(value, iterable.span.clone()))
                            .collect(),
                    );
                }
                Some("std.stream.zip" | "std.stream.zip_exact") => {
                    let mut streams = Vec::new();
                    for argument in arguments {
                        streams.push((
                            argument.name.clone(),
                            self.materialize_stream_source(
                                summary,
                                &argument.value,
                                substitutions,
                                configured_bindings,
                                run_exports,
                                input_streams,
                                stack,
                            )?,
                        ));
                    }
                    let count = streams.iter().map(|(_, items)| items.len()).min()?;
                    if callee.path().as_deref() == Some("std.stream.zip_exact")
                        && streams.iter().any(|(_, items)| items.len() != count)
                    {
                        return None;
                    }
                    return Some(
                        (0..count)
                            .map(|index| Expr {
                                kind: ExprKind::Call {
                                    callee: Box::new(Expr::name("tuple", iterable.span.clone())),
                                    arguments: streams
                                        .iter()
                                        .enumerate()
                                        .map(|(stream_index, (name, items))| CallArgument {
                                            name: name
                                                .clone()
                                                .or_else(|| Some(format!("item{stream_index}"))),
                                            value: items[index].clone(),
                                            span: iterable.span.clone(),
                                        })
                                        .collect(),
                                },
                                span: iterable.span.clone(),
                            })
                            .collect(),
                    );
                }
                _ => {}
            }
        }
        let _ = process_name;
        self.materialize_stream_source(
            summary,
            &iterable,
            substitutions,
            configured_bindings,
            run_exports,
            input_streams,
            stack,
        )
    }

    #[allow(clippy::too_many_arguments)]
    fn materialize_effects(
        &self,
        process_name: &str,
        summary: &ProcessSummary,
        effects: &[EffectTemplate],
        output: &str,
        values: &mut IndexMap<String, Expr>,
        substitutions: &IndexMap<String, Expr>,
        configured_bindings: &IndexMap<String, ConfiguredTemplate>,
        run_exports: &HashMap<String, Expr>,
        input_streams: &HashMap<String, Vec<Expr>>,
        stack: &mut Vec<(String, String)>,
        result: &mut Vec<Expr>,
    ) -> Option<()> {
        for effect in effects {
            match effect {
                EffectTemplate::Bind { name, value, .. } => {
                    values.insert(
                        name.clone(),
                        self.materialize_effect_expression(value, values),
                    );
                }
                EffectTemplate::Assign { target, value, .. } => {
                    if let Some(name) = target.as_name() {
                        values.insert(
                            name.to_owned(),
                            self.materialize_effect_expression(value, values),
                        );
                    }
                }
                EffectTemplate::Yield { target, value, .. } if target.as_name() == Some(output) => {
                    result.push(self.materialize_effect_expression(value, values));
                }
                EffectTemplate::For {
                    binding,
                    iterable,
                    body,
                    ..
                } => {
                    let items = self.materialize_iterable(
                        process_name,
                        summary,
                        iterable,
                        values,
                        substitutions,
                        configured_bindings,
                        run_exports,
                        input_streams,
                        stack,
                    )?;
                    for item in items {
                        let prior = values.insert(binding.clone(), item);
                        self.materialize_effects(
                            process_name,
                            summary,
                            body,
                            output,
                            values,
                            substitutions,
                            configured_bindings,
                            run_exports,
                            input_streams,
                            stack,
                            result,
                        )?;
                        if let Some(prior) = prior {
                            values.insert(binding.clone(), prior);
                        } else {
                            values.shift_remove(binding);
                        }
                    }
                }
                EffectTemplate::Repeat { count, body, .. } => {
                    let count = self.materialize_effect_expression(count, values);
                    let count = self.exact_nonnegative_count(&count)?;
                    for _ in 0..count {
                        self.materialize_effects(
                            process_name,
                            summary,
                            body,
                            output,
                            values,
                            substitutions,
                            configured_bindings,
                            run_exports,
                            input_streams,
                            stack,
                            result,
                        )?;
                    }
                }
                EffectTemplate::Branch {
                    condition,
                    then_body,
                    else_body,
                    ..
                } => {
                    let condition = self.materialize_effect_expression(condition, values);
                    let body = if self.evaluate_predicate(&condition)? {
                        then_body
                    } else {
                        else_body
                    };
                    self.materialize_effects(
                        process_name,
                        summary,
                        body,
                        output,
                        values,
                        substitutions,
                        configured_bindings,
                        run_exports,
                        input_streams,
                        stack,
                        result,
                    )?;
                }
                _ => {}
            }
        }
        Some(())
    }

    #[allow(clippy::too_many_arguments)]
    fn materialize_process_stream(
        &self,
        process_name: &str,
        summary: &ProcessSummary,
        output: &str,
        substitutions: &IndexMap<String, Expr>,
        configured_bindings: &IndexMap<String, ConfiguredTemplate>,
        run_exports: &HashMap<String, Expr>,
        input_streams: &HashMap<String, Vec<Expr>>,
        stack: &mut Vec<(String, String)>,
    ) -> Option<Vec<Expr>> {
        let key = (process_name.to_owned(), output.to_owned());
        if stack.contains(&key) {
            return None;
        }
        stack.push(key);
        let mut values = substitutions.clone();
        let mut result = Vec::new();
        let materialized = self.materialize_effects(
            process_name,
            summary,
            &summary.effects,
            output,
            &mut values,
            substitutions,
            configured_bindings,
            run_exports,
            input_streams,
            stack,
            &mut result,
        );
        stack.pop();
        materialized.map(|_| result)
    }

    /// Computes the causal timing of the lexical child runs in one configured
    /// process instance without applying capacity limits. This is used to
    /// release a physical loop only after the expression producing its jobs is
    /// actually available; the capacity scheduler then starts from that
    /// release point instead of silently treating every job as present at the
    /// process origin.
    fn local_run_timings(
        &self,
        summary: &ProcessSummary,
        configured_bindings: &IndexMap<String, ConfiguredTemplate>,
        run_exports: &HashMap<String, Expr>,
        stream_handoffs: &HashMap<(String, String), Option<BigRational>>,
        needed: &BTreeSet<String>,
    ) -> Option<HashMap<String, (BigRational, BigRational)>> {
        let run_names = summary
            .runs
            .keys()
            .filter(|name| needed.contains(*name))
            .cloned()
            .collect::<Vec<_>>();
        let run_instances = run_names
            .iter()
            .enumerate()
            .map(|(index, name)| (name.clone(), index))
            .collect::<HashMap<_, _>>();
        let mut stages = Vec::with_capacity(run_names.len());
        let mut operations = Vec::with_capacity(run_names.len());

        for name in &run_names {
            let run = resolve_run_template(summary.runs.get(name)?, configured_bindings);
            let has_duration = self
                .resolved_members
                .get(&run.process)
                .is_some_and(|members| members.contains_key("duration"));
            let (duration, dimension, duration_display) = if has_duration {
                let duration_expression = run_exports.get(&format!("{name}.duration"))?;
                let (duration, dimension) = self.constant_quantity(duration_expression)?;
                (duration, dimension, duration_expression.display())
            } else {
                (
                    BigRational::zero(),
                    self.registry.units.get("Second")?.dimension.clone(),
                    "0 Second".to_owned(),
                )
            };
            if duration < BigRational::zero()
                || self
                    .registry
                    .units
                    .get("Second")
                    .is_none_or(|unit| unit.dimension != dimension)
            {
                return None;
            }
            stages.push(ConcreteScheduleStage {
                name: name.clone(),
                duration,
                job_durations: None,
                duration_display,
                parallel_capacity: 1,
                resources: Vec::new(),
            });

            let mut dependencies = Vec::new();
            for value in run.config.values().chain(run.arguments.values()) {
                dependencies.extend(
                    schedule_expression_producers(value, &run_instances, &HashMap::new())
                        .into_iter()
                        .map(ScheduleDependency::Completion),
                );
            }
            operations.push(ConcreteScheduleOperation {
                stage: operations.len(),
                dependencies,
            });
        }

        for (source, target, _) in &summary.wires {
            let Some(target_path) = target.path() else {
                continue;
            };
            let Some(target_run) = target_path.split('.').next() else {
                continue;
            };
            let Some(target_index) = run_instances.get(target_run).copied() else {
                continue;
            };
            let producers = schedule_expression_producers(source, &run_instances, &HashMap::new());
            let stream_source = match &source.kind {
                ExprKind::Member { value, member } => value.as_name().and_then(|producer| {
                    let run =
                        resolve_run_template(summary.runs.get(producer)?, configured_bindings);
                    let ty = self
                        .resolved_members
                        .get(&run.process)?
                        .get(member)?
                        .ty
                        .clone();
                    matches!(ty, Type::Stream(_)).then(|| (producer.to_owned(), member.clone()))
                }),
                _ => None,
            };
            for producer in producers {
                if let Some(stream_source) = &stream_source {
                    match stream_handoffs.get(stream_source).cloned().flatten() {
                        Some(delay) => {
                            operations[target_index].dependencies.push(
                                ScheduleDependency::Handoff {
                                    operation: producer,
                                    delay,
                                },
                            );
                            operations[target_index]
                                .dependencies
                                .push(ScheduleDependency::Close(producer));
                        }
                        None => operations[target_index]
                            .dependencies
                            .push(ScheduleDependency::Completion(producer)),
                    }
                } else {
                    operations[target_index]
                        .dependencies
                        .push(ScheduleDependency::Completion(producer));
                }
            }
        }
        for (first, second) in &summary.ordering {
            if let (Some(first), Some(second)) =
                (run_instances.get(first), run_instances.get(second))
            {
                operations[*second]
                    .dependencies
                    .push(ScheduleDependency::Completion(*first));
            }
        }
        for operation in &mut operations {
            dedup_schedule_dependencies(&mut operation.dependencies);
        }

        let scheduled = schedule_effect_jobs(1, &operations, &stages);
        if !scheduled.complete {
            return None;
        }
        Some(
            run_names
                .into_iter()
                .zip(scheduled.stages)
                .map(|(name, stage)| (name, (stage.earliest_start, stage.latest_finish)))
                .collect(),
        )
    }

    fn local_run_timing_for(
        &self,
        target: &str,
        summary: &ProcessSummary,
        configured_bindings: &IndexMap<String, ConfiguredTemplate>,
        run_exports: &HashMap<String, Expr>,
        stream_handoffs: &HashMap<(String, String), Option<BigRational>>,
    ) -> Option<(BigRational, BigRational)> {
        let run_names = summary.runs.keys().cloned().collect::<Vec<_>>();
        let run_instances = run_names
            .iter()
            .enumerate()
            .map(|(index, name)| (name.clone(), index))
            .collect::<HashMap<_, _>>();
        let mut needed = BTreeSet::from([target.to_owned()]);
        let mut changed = true;
        while changed {
            changed = false;
            for run_name in needed.clone() {
                let run = resolve_run_template(summary.runs.get(&run_name)?, configured_bindings);
                for value in run.config.values().chain(run.arguments.values()) {
                    for producer in
                        schedule_expression_producers(value, &run_instances, &HashMap::new())
                    {
                        if needed.insert(run_names.get(producer)?.clone()) {
                            changed = true;
                        }
                    }
                }
                for (source, target, _) in &summary.wires {
                    if target
                        .path()
                        .as_deref()
                        .and_then(|path| path.split('.').next())
                        != Some(run_name.as_str())
                    {
                        continue;
                    }
                    for producer in
                        schedule_expression_producers(source, &run_instances, &HashMap::new())
                    {
                        if needed.insert(run_names.get(producer)?.clone()) {
                            changed = true;
                        }
                    }
                }
                for (first, second) in &summary.ordering {
                    if second == &run_name && needed.insert(first.clone()) {
                        changed = true;
                    }
                }
            }
        }
        self.local_run_timings(
            summary,
            configured_bindings,
            run_exports,
            stream_handoffs,
            &needed,
        )?
        .remove(target)
    }

    fn effect_schedule_release_offset(
        &self,
        iterable: &Expr,
        summary: &ProcessSummary,
        configured_bindings: &IndexMap<String, ConfiguredTemplate>,
        run_exports: &HashMap<String, Expr>,
        stream_handoffs: &HashMap<(String, String), Option<BigRational>>,
    ) -> Option<BigRational> {
        let run_instances = summary
            .runs
            .keys()
            .enumerate()
            .map(|(index, name)| (name.clone(), index))
            .collect::<HashMap<_, _>>();
        let run_names = summary.runs.keys().cloned().collect::<Vec<_>>();
        let producers = schedule_expression_producers(iterable, &run_instances, &HashMap::new());
        if producers.is_empty() {
            return Some(BigRational::zero());
        }
        let mut needed = producers
            .iter()
            .filter_map(|producer| run_names.get(*producer).cloned())
            .collect::<BTreeSet<_>>();
        let mut changed = true;
        while changed {
            changed = false;
            for run_name in needed.clone() {
                let run = resolve_run_template(summary.runs.get(&run_name)?, configured_bindings);
                for value in run.config.values().chain(run.arguments.values()) {
                    for producer in
                        schedule_expression_producers(value, &run_instances, &HashMap::new())
                    {
                        if needed.insert(run_names.get(producer)?.clone()) {
                            changed = true;
                        }
                    }
                }
                for (source, target, _) in &summary.wires {
                    if target
                        .path()
                        .as_deref()
                        .and_then(|path| path.split('.').next())
                        != Some(run_name.as_str())
                    {
                        continue;
                    }
                    for producer in
                        schedule_expression_producers(source, &run_instances, &HashMap::new())
                    {
                        if needed.insert(run_names.get(producer)?.clone()) {
                            changed = true;
                        }
                    }
                }
                for (first, second) in &summary.ordering {
                    if second == &run_name && needed.insert(first.clone()) {
                        changed = true;
                    }
                }
            }
        }
        let timings = self.local_run_timings(
            summary,
            configured_bindings,
            run_exports,
            stream_handoffs,
            &needed,
        )?;

        let direct_stream = match &iterable.kind {
            ExprKind::Member { value, member } => {
                value.as_name().map(|run| (run.to_owned(), member.clone()))
            }
            _ => None,
        };
        let mut release = BigRational::zero();
        for producer in producers {
            let name = run_names.get(producer)?;
            let (start, finish) = timings.get(name)?;
            let available = direct_stream
                .as_ref()
                .filter(|(run, _)| run == name)
                .and_then(|stream| stream_handoffs.get(stream).cloned().flatten())
                .map(|delay| start + delay)
                .unwrap_or_else(|| finish.clone());
            release = release.max(available);
        }
        Some(release)
    }

    #[allow(clippy::too_many_arguments)]
    fn build_effect_schedule_operations(
        &mut self,
        effects: &[EffectTemplate],
        substitutions: &IndexMap<String, Expr>,
        run_exports: &HashMap<String, Expr>,
        item: &ItemRef,
        summary: &ProcessSummary,
        operations: &mut Vec<SymbolicScheduleOperation>,
        run_instances: &mut HashMap<String, usize>,
        value_producers: &mut HashMap<String, BTreeSet<usize>>,
        inherited_predecessors: &BTreeSet<usize>,
        stream_handoffs: &HashMap<(String, String), Option<BigRational>>,
        reported_repetitions: &mut u64,
    ) -> bool {
        for effect in effects {
            match effect {
                EffectTemplate::Run { name, .. } => {
                    let mut dependencies = inherited_predecessors
                        .iter()
                        .copied()
                        .map(ScheduleDependency::Completion)
                        .collect::<Vec<_>>();
                    if let Some(run) = summary.runs.get(name) {
                        for value in run.config.values().chain(run.arguments.values()) {
                            dependencies.extend(
                                schedule_expression_producers(
                                    value,
                                    run_instances,
                                    value_producers,
                                )
                                .into_iter()
                                .map(ScheduleDependency::Completion),
                            );
                        }
                    }
                    dedup_schedule_dependencies(&mut dependencies);
                    let index = operations.len();
                    operations.push(SymbolicScheduleOperation {
                        run: name.clone(),
                        dependencies,
                    });
                    run_instances.insert(name.clone(), index);
                }
                EffectTemplate::Repeat { count, body, span } => {
                    let count = instantiate_expression(count, substitutions, run_exports);
                    let Some(count) = self.exact_nonnegative_count(&count) else {
                        self.push_error(
                            &item.file_key,
                            span.clone(),
                            "E0720",
                            "physical repeat count must resolve to a nonnegative compile-time integer",
                        );
                        return false;
                    };
                    *reported_repetitions = (*reported_repetitions).max(count);
                    // A repeat elaborates copies of the body into one dataflow
                    // graph. Do not serialize a copy behind every operation in
                    // the preceding copy: assignments and wires update
                    // `value_producers`, so a loop-carried value creates the
                    // precise cross-iteration edge that it actually needs.
                    // All copies still remain in `operations`, and therefore
                    // contribute to the enclosing schedule's completion.
                    for _ in 0..count {
                        if !self.build_effect_schedule_operations(
                            body,
                            substitutions,
                            run_exports,
                            item,
                            summary,
                            operations,
                            run_instances,
                            value_producers,
                            inherited_predecessors,
                            stream_handoffs,
                            reported_repetitions,
                        ) {
                            return false;
                        }
                        if operations.len() > 1_000_000 {
                            self.push_error(
                                &item.file_key,
                                span.clone(),
                                "E0722",
                                "physical repeat exceeds the v1 limit of 1,000,000 operations",
                            );
                            return false;
                        }
                    }
                }
                EffectTemplate::For {
                    binding,
                    iterable,
                    body,
                    span,
                } => {
                    let _ = binding;
                    if !effects_contain_run(body) {
                        continue;
                    }
                    let count = iteration_count_expression(iterable, span)
                        .unwrap_or_else(|| cardinality_expression(iterable, span));
                    let count = instantiate_expression(&count, substitutions, run_exports);
                    let Some(count) = self.exact_nonnegative_count(&count) else {
                        self.push_error(
                            &item.file_key,
                            span.clone(),
                            "E0720",
                            "nested physical loop count must resolve to a nonnegative compile-time integer",
                        );
                        return false;
                    };
                    let mut barrier = inherited_predecessors.clone();
                    for _ in 0..count {
                        let start = operations.len();
                        if !self.build_effect_schedule_operations(
                            body,
                            substitutions,
                            run_exports,
                            item,
                            summary,
                            operations,
                            run_instances,
                            value_producers,
                            &barrier,
                            stream_handoffs,
                            reported_repetitions,
                        ) {
                            return false;
                        }
                        barrier = (start..operations.len()).collect();
                    }
                }
                EffectTemplate::Branch {
                    condition,
                    then_body,
                    else_body,
                    span,
                } => {
                    let condition = instantiate_expression(condition, substitutions, run_exports);
                    let selected = match self.evaluate_predicate(&condition) {
                        Some(true) => then_body,
                        Some(false) => else_body,
                        None => {
                            self.push_error(
                                &item.file_key,
                                span.clone(),
                                "E0729",
                                "a physical schedule branch must resolve at compile time",
                            );
                            return false;
                        }
                    };
                    if !self.build_effect_schedule_operations(
                        selected,
                        substitutions,
                        run_exports,
                        item,
                        summary,
                        operations,
                        run_instances,
                        value_producers,
                        inherited_predecessors,
                        stream_handoffs,
                        reported_repetitions,
                    ) {
                        return false;
                    }
                }
                EffectTemplate::Wire {
                    source,
                    target,
                    span,
                } => {
                    let _ = span;
                    if let Some(target_path) = target.path()
                        && let Some(target_run) = target_path.split('.').next()
                        && let Some(target_index) = run_instances.get(target_run).copied()
                    {
                        let producers =
                            schedule_expression_producers(source, run_instances, value_producers);
                        let stream_source = match &source.kind {
                            ExprKind::Member { value, member } => value.as_name().and_then(|run| {
                                let process = summary.runs.get(run).map(|run| &run.process)?;
                                let ty =
                                    self.resolved_members.get(process)?.get(member)?.ty.clone();
                                matches!(ty, Type::Stream(_))
                                    .then(|| (run.to_owned(), member.clone()))
                            }),
                            _ => None,
                        };
                        if let Some(stream_source) = stream_source {
                            for producer in producers {
                                match stream_handoffs.get(&stream_source).cloned().flatten() {
                                    Some(delay) => {
                                        operations[target_index].dependencies.push(
                                            ScheduleDependency::Handoff {
                                                operation: producer,
                                                delay,
                                            },
                                        );
                                        operations[target_index]
                                            .dependencies
                                            .push(ScheduleDependency::Close(producer));
                                    }
                                    None => operations[target_index]
                                        .dependencies
                                        .push(ScheduleDependency::Completion(producer)),
                                }
                            }
                        } else {
                            operations[target_index]
                                .dependencies
                                .extend(producers.into_iter().map(ScheduleDependency::Completion));
                        }
                        dedup_schedule_dependencies(&mut operations[target_index].dependencies);
                    }
                }
                EffectTemplate::Assign {
                    target,
                    value,
                    span,
                } => {
                    let _ = span;
                    if let Some(target) = target.path() {
                        value_producers.insert(
                            target,
                            schedule_expression_producers(value, run_instances, value_producers),
                        );
                    }
                }
                EffectTemplate::Bind { name, value, span } => {
                    let _ = span;
                    value_producers.insert(
                        name.clone(),
                        schedule_expression_producers(value, run_instances, value_producers),
                    );
                }
                EffectTemplate::Yield {
                    target,
                    value,
                    after,
                    span,
                } => {
                    let _ = (target, value, after, span);
                }
                EffectTemplate::Before {
                    first,
                    second,
                    span,
                } => {
                    let _ = span;
                    if let (Some(first), Some(second)) =
                        (run_instances.get(first), run_instances.get(second))
                    {
                        operations[*second]
                            .dependencies
                            .push(ScheduleDependency::Completion(*first));
                        dedup_schedule_dependencies(&mut operations[*second].dependencies);
                    }
                }
            }
        }
        true
    }

    fn exact_positive_count(&self, expression: &Expr) -> Option<u64> {
        let (value, dimension) = self.constant_quantity(expression)?;
        if !dimension.is_scalar() || !value.is_integer() || value <= BigRational::zero() {
            return None;
        }
        value.to_integer().to_u64()
    }

    fn exact_nonnegative_count(&self, expression: &Expr) -> Option<u64> {
        let (value, dimension) = self.constant_quantity(expression)?;
        if !dimension.is_scalar() || !value.is_integer() || value < BigRational::zero() {
            return None;
        }
        value.to_integer().to_u64()
    }

    fn resource_amount(
        &self,
        ty: &Type,
        substitutions: &IndexMap<String, Expr>,
    ) -> Option<(BigRational, Dimension)> {
        let Type::Resource { name, claim, .. } = ty else {
            return None;
        };
        let expression = if let Some(claim) = claim {
            crate::parser::parse_expression(claim).ok()?
        } else {
            let item = self.resources.get(name)?;
            let Item::Resource(resource) = &item.item else {
                return None;
            };
            resource.capacity.clone()
        };
        self.constant_quantity(&normalize_expr(&expression, substitutions))
    }

    fn instantiated_resource_pool(
        &self,
        endpoint_path: &str,
        fallback_type: &Type,
        substitutions: &IndexMap<String, Expr>,
    ) -> Option<(String, (BigRational, Dimension))> {
        if let Some((origin, capacity, dimension, _)) =
            self.resource_pool_bindings.get(endpoint_path)
        {
            return Some((origin.clone(), (capacity.clone(), dimension.clone())));
        }
        self.resource_amount(fallback_type, substitutions)
            .map(|capacity| (endpoint_path.to_owned(), capacity))
    }

    /// Recognize the deliberately narrow resource-construction expression
    /// `ResourceType[capacity]`. Context validation keeps this from becoming a
    /// general first-class resource value.
    fn resource_constructor(&self, expression: &Expr) -> Option<Type> {
        let ExprKind::Index {
            value,
            index: Some(index),
            wildcard: false,
        } = &expression.kind
        else {
            return None;
        };
        let name = value.as_name()?;
        let capacity = self.registry.resources.get(name)?.clone();
        Some(Type::Resource {
            name: name.to_owned(),
            capacity: Box::new(capacity),
            claim: Some(index.display()),
        })
    }

    fn resource_constructor_matches_export(
        &self,
        expected: &Type,
        actual: &Type,
        environment: &TypeEnv,
    ) -> bool {
        let (
            Type::Resource {
                name: expected_name,
                claim: expected_claim,
                ..
            },
            Type::Resource {
                name: actual_name,
                claim: actual_claim,
                ..
            },
        ) = (expected, actual)
        else {
            return false;
        };
        if expected_name != actual_name {
            return false;
        }
        let resolve = |claim: &Option<String>, resource_name: &str| {
            claim
                .as_deref()
                .and_then(|claim| crate::parser::parse_expression(claim).ok())
                .or_else(|| {
                    let item = self.resources.get(resource_name)?;
                    let Item::Resource(resource) = &item.item else {
                        return None;
                    };
                    Some(resource.capacity.clone())
                })
                .map(|expression| normalize_expr(&expression, &environment.expressions))
        };
        let (Some(expected), Some(actual)) = (
            resolve(expected_claim, expected_name),
            resolve(actual_claim, actual_name),
        ) else {
            return false;
        };
        match (
            self.constant_quantity(&expected),
            self.constant_quantity(&actual),
        ) {
            (Some(left), Some(right)) => left == right,
            _ => expected.display() == actual.display(),
        }
    }

    fn declared_resource_amount(&self, name: &str) -> Option<(BigRational, Dimension)> {
        let item = self.resources.get(name)?;
        let Item::Resource(resource) = &item.item else {
            return None;
        };
        self.constant_quantity(&resource.capacity)
    }

    fn format_schedule_seconds(&self, seconds: &BigRational) -> String {
        let unit_name = self.schedule_calendar_unit();
        let unit = &self.registry.units[unit_name];
        format!(
            "{} {unit_name}",
            format_exact_rational(&unit.from_canonical(seconds))
        )
    }

    /// The coarsest calendar unit the *model* itself introduced, for rendering
    /// schedule totals.
    ///
    /// Deliberately keyed off the author's own `unit` declarations rather than
    /// the registry: `Day` and `Year` are now built in, so registry presence
    /// would report every schedule in years, including nine-second ones.
    fn schedule_calendar_unit(&self) -> &'static str {
        if self.model.units.contains_key("Year") {
            "Year"
        } else if self.model.units.contains_key("Day") {
            "Day"
        } else {
            "Second"
        }
    }

    /// `Some(name)` when the model declared that calendar unit itself, which is
    /// the signal that its author thinks in those terms. See
    /// [`Self::schedule_calendar_unit`] for why the registry cannot answer this.
    fn authored_calendar_unit(&self, name: &'static str) -> Option<&'static str> {
        self.model.units.contains_key(name).then_some(name)
    }

    /// `TemporalMetric` retains author-facing units for reports, while the
    /// browser's Gantt needs one exact scale to place bars. All temporal
    /// metrics have already been checked as Time-dimensional.
    fn displayed_time_seconds(&self, display: &str) -> Option<String> {
        let (number, unit_name) = display.split_once(' ')?;
        let value = if let Some((numerator, denominator)) = number.split_once('/') {
            BigRational::new(numerator.parse().ok()?, denominator.parse().ok()?)
        } else {
            parse_rational(number)?
        };
        let unit = self.registry.units.get(unit_name)?;
        let time = self.registry.units.get("Second")?;
        (unit.dimension == time.dimension)
            .then(|| format_exact_rational(&unit.to_canonical(&value)))
    }

    /// Returns only obligations that can make this particular time aggregate
    /// omit work.  General missing inputs remain visible in the ordinary
    /// obligation report; they make the timing symbolic rather than being
    /// silently converted into a zero-duration contribution.
    fn temporal_completion_obligations(&self, metric_path: &str) -> Vec<String> {
        let Some((instance_path, metric_name)) = metric_path.rsplit_once('.') else {
            return Vec::new();
        };
        let scope = format!("{instance_path}.");
        let suffix = format!(".{metric_name}");
        let mut paths = self
            .report
            .obligations
            .iter()
            .filter(|obligation| {
                obligation.path.starts_with(&scope)
                    && obligation.path.ends_with(&suffix)
                    && matches!(
                        obligation.kind.as_str(),
                        "monitored_metric_coverage" | "missing_export"
                    )
            })
            .map(|obligation| obligation.path.clone())
            .collect::<Vec<_>>();
        paths.sort();
        paths.dedup();
        paths
    }

    fn instantiate_run_exports(
        &self,
        summary: &ProcessSummary,
        configured_bindings: &IndexMap<String, ConfiguredTemplate>,
        substitutions: &IndexMap<String, Expr>,
    ) -> HashMap<String, Expr> {
        let mut replacements = self.instantiate_run_exports_inner(
            summary,
            configured_bindings,
            substitutions,
            &mut Vec::new(),
        );
        for (stream, cardinality) in &summary.stream_cardinalities {
            replacements.insert(
                format!("cardinality({stream})"),
                instantiate_expression(cardinality, substitutions, &replacements),
            );
        }
        replacements
    }

    fn instantiate_run_exports_inner(
        &self,
        summary: &ProcessSummary,
        configured_bindings: &IndexMap<String, ConfiguredTemplate>,
        substitutions: &IndexMap<String, Expr>,
        stack: &mut Vec<String>,
    ) -> HashMap<String, Expr> {
        let inputs = run_inputs(summary);
        let mut replacements = HashMap::new();
        for (run_name, run) in &summary.runs {
            let run = resolve_run_template(run, configured_bindings);
            let mut child_substitutions = IndexMap::new();
            for (name, value) in run.config.iter().chain(&run.arguments) {
                child_substitutions.insert(
                    name.clone(),
                    instantiate_expression(value, substitutions, &HashMap::new()),
                );
            }
            if let Some(child_members) = self.resolved_members.get(&run.process) {
                for (port, member) in child_members {
                    if matches!(member.role, MemberRole::Gets | MemberRole::Uses)
                        && let Some(source) = inputs.get(&(run_name.clone(), port.clone()))
                    {
                        child_substitutions.insert(
                            port.clone(),
                            instantiate_expression(source, substitutions, &HashMap::new()),
                        );
                    }
                }
            }
            let Some(child_summary) = self.summaries.get(&run.process) else {
                continue;
            };
            let child_run_replacements = if stack.iter().any(|process| process == &run.process) {
                HashMap::new()
            } else {
                stack.push(run.process.clone());
                let replacements = self.instantiate_run_exports_inner(
                    child_summary,
                    &run.configured_arguments,
                    &child_substitutions,
                    stack,
                );
                stack.pop();
                replacements
            };
            for (export, value) in &child_summary.export_values {
                replacements.insert(
                    format!("{run_name}.{export}"),
                    instantiate_expression(value, &child_substitutions, &child_run_replacements),
                );
            }
            for (stream, cardinality) in &child_summary.stream_cardinalities {
                replacements.insert(
                    format!("cardinality({run_name}.{stream})"),
                    instantiate_expression(
                        cardinality,
                        &child_substitutions,
                        &child_run_replacements,
                    ),
                );
            }
        }
        replacements
    }

    fn emit_derived_constraints(
        &mut self,
        process_name: &str,
        object_path: &str,
        configured_bindings: &IndexMap<String, ConfiguredTemplate>,
        templates: &[Constraint],
        stack: &mut Vec<String>,
    ) {
        if stack.iter().any(|name| name == process_name) {
            return;
        }
        let Some(summary) = self.summaries.get(process_name).cloned() else {
            return;
        };
        stack.push(process_name.to_owned());
        let prefix = format!("{process_name}.");
        for template in templates {
            let Some(suffix) = template.path.strip_prefix(&prefix) else {
                continue;
            };
            let mut constraint = template.clone();
            constraint.path = format!("{object_path}.{suffix}");
            constraint.id = format!(
                "constraint:{}",
                stable_fingerprint(&format!("{}:{}", constraint.path, constraint.predicate))
            );
            self.report.constraints.push(constraint);
        }
        for (run_name, run) in &summary.runs {
            let run = resolve_run_template(run, configured_bindings);
            self.emit_derived_constraints(
                &run.process,
                &format!("{object_path}.{run_name}"),
                &run.configured_arguments,
                templates,
                stack,
            );
        }
        stack.pop();
    }

    #[allow(clippy::too_many_arguments)]
    fn emit_process_constraints(
        &mut self,
        process_name: &str,
        object_path: &str,
        substitutions: &IndexMap<String, Expr>,
        configured_bindings: &IndexMap<String, ConfiguredTemplate>,
        path_replacements: &HashMap<String, Expr>,
        constraint_replacements: &HashMap<usize, HashMap<String, Expr>>,
        instance_source: Option<(String, Span)>,
        stack: &mut Vec<String>,
    ) {
        if stack.iter().any(|name| name == process_name) {
            return;
        }
        let Some(summary) = self.summaries.get(process_name).cloned() else {
            return;
        };
        let Some(item) = self.processes.get(process_name).cloned() else {
            return;
        };
        stack.push(process_name.to_owned());
        let mut local_path_replacements = path_replacements.clone();
        local_path_replacements.extend(self.instantiate_run_exports(
            &summary,
            configured_bindings,
            substitutions,
        ));
        let path_replacements = &local_path_replacements;

        for (index, constraint) in summary.template_constraints.iter().enumerate() {
            let filtered_substitutions;
            let substitutions = if let Some((binding, _)) = &constraint.stream_binding {
                filtered_substitutions = substitutions
                    .iter()
                    .filter(|(name, _)| *name != binding)
                    .map(|(name, value)| (name.clone(), value.clone()))
                    .collect();
                &filtered_substitutions
            } else {
                substitutions
            };
            let expression =
                instantiate_expression(&constraint.expression, substitutions, path_replacements);
            let expression = constraint_replacements
                .get(&index)
                .map_or(expression.clone(), |replacements| {
                    substitute_paths(&expression, replacements)
                });
            let path = format!("{object_path}.requirement[{index}]");
            let (file_key, span) = instance_source
                .clone()
                .unwrap_or_else(|| (item.file_key.clone(), constraint.span.clone()));
            match self.evaluate_predicate(&expression) {
                Some(true) => continue,
                Some(false) => {
                    self.push_error(
                        &file_key,
                        span,
                        "E0901",
                        format!(
                            "constraint {path} is statically false: {}",
                            expression.display()
                        ),
                    );
                }
                None => {
                    let predicate = expression.display();
                    self.report.constraints.push(Constraint {
                        id: format!(
                            "constraint:{}",
                            stable_fingerprint(&format!("{path}:{predicate}"))
                        ),
                        path,
                        phase: constraint.phase.clone(),
                        predicate,
                        status: "residual".into(),
                        span: self.span(&file_key, span),
                        dependencies: expression_names(&expression),
                    });
                }
            }
        }

        let inputs = run_inputs(&summary);
        for (run_name, run) in &summary.runs {
            let run = resolve_run_template(run, configured_bindings);
            let mut child_substitutions = IndexMap::new();
            for (name, value) in run.config.iter().chain(&run.arguments) {
                child_substitutions.insert(
                    name.clone(),
                    instantiate_expression(value, substitutions, path_replacements),
                );
            }
            if let Some(child_members) = self.resolved_members.get(&run.process) {
                for (port, member) in child_members {
                    if matches!(member.role, MemberRole::Gets | MemberRole::Uses)
                        && let Some(source) = inputs.get(&(run_name.clone(), port.clone()))
                    {
                        child_substitutions.insert(
                            port.clone(),
                            instantiate_expression(source, substitutions, path_replacements),
                        );
                    }
                }
            }
            let child_replacements = self.stream_constraint_replacements(
                &summary,
                run_name,
                &run,
                &inputs,
                substitutions,
                configured_bindings,
                path_replacements,
            );
            self.emit_process_constraints(
                &run.process,
                &format!("{object_path}.{run_name}"),
                &child_substitutions,
                &run.configured_arguments,
                path_replacements,
                &child_replacements,
                Some((item.file_key.clone(), run.span.clone())),
                stack,
            );
        }
        stack.pop();
    }

    #[allow(clippy::too_many_arguments)]
    fn stream_constraint_replacements(
        &self,
        parent_summary: &ProcessSummary,
        run_name: &str,
        run: &RunTemplate,
        inputs: &HashMap<(String, String), Expr>,
        substitutions: &IndexMap<String, Expr>,
        configured_bindings: &IndexMap<String, ConfiguredTemplate>,
        path_replacements: &HashMap<String, Expr>,
    ) -> HashMap<usize, HashMap<String, Expr>> {
        let Some(child_summary) = self.summaries.get(&run.process) else {
            return HashMap::new();
        };
        let mut replacements = HashMap::<usize, HashMap<String, Expr>>::new();
        for (constraint_index, constraint) in child_summary.template_constraints.iter().enumerate()
        {
            let Some((binding, port)) = &constraint.stream_binding else {
                continue;
            };
            let Some(source) = inputs.get(&(run_name.to_owned(), port.clone())) else {
                continue;
            };
            let ExprKind::Member {
                value,
                member: output,
            } = &source.kind
            else {
                continue;
            };
            let Some(producer_name) = value.as_name() else {
                continue;
            };
            let Some(producer_run) = parent_summary.runs.get(producer_name) else {
                continue;
            };
            let producer_run = resolve_run_template(producer_run, configured_bindings);
            let Some(producer_summary) = self.summaries.get(&producer_run.process) else {
                continue;
            };
            let Some(yield_templates) = producer_summary.yields.get(output) else {
                continue;
            };
            // A stream guard is universal over emitted items. Until conditional
            // predicates are represented in HIR, substituting one of several
            // yield sites would be unsound: a later satisfying item could hide an
            // earlier contradiction. Retain the symbolic guard conservatively.
            let [yield_template] = yield_templates.as_slice() else {
                continue;
            };
            let mut producer_substitutions = IndexMap::new();
            for (name, value) in producer_run.config.iter().chain(&producer_run.arguments) {
                producer_substitutions.insert(
                    name.clone(),
                    instantiate_expression(value, substitutions, path_replacements),
                );
            }
            if let Some(producer_members) = self.resolved_members.get(&producer_run.process) {
                for (port, member) in producer_members {
                    if matches!(member.role, MemberRole::Gets | MemberRole::Uses)
                        && let Some(source) = inputs.get(&(producer_name.to_owned(), port.clone()))
                    {
                        producer_substitutions.insert(
                            port.clone(),
                            instantiate_expression(source, substitutions, path_replacements),
                        );
                    }
                }
            }
            for (field, value) in &yield_template.fields {
                replacements.entry(constraint_index).or_default().insert(
                    format!("{binding}.{field}"),
                    instantiate_expression(value, &producer_substitutions, path_replacements),
                );
            }
        }
        replacements
    }

    fn instantiate_open_process_obligations(
        &mut self,
        parent_item: &ItemRef,
        parent_name: &str,
        run_name: &str,
        run: &RunTemplate,
    ) {
        let members = self
            .resolved_members
            .get(&run.process)
            .cloned()
            .unwrap_or_default();
        let defined = self
            .summaries
            .get(&run.process)
            .map(|summary| {
                summary
                    .export_values
                    .keys()
                    .cloned()
                    .collect::<BTreeSet<_>>()
            })
            .unwrap_or_default();
        for (name, member) in members {
            if member.role == MemberRole::Export && !defined.contains(&name) {
                self.push_obligation(
                    &parent_item.file_key,
                    run.span.clone(),
                    "open_process_export",
                    format!("{parent_name}.{run_name}.{name}"),
                    member.ty.to_string(),
                    format!("open process {} does not define export {name}", run.process),
                    None,
                );
            }
        }
    }

    fn bubble_child_obligations(
        &mut self,
        parent_item: &ItemRef,
        parent_name: &str,
        run_name: &str,
        child_name: &str,
        span: &Span,
    ) {
        let child_prefix = format!("{child_name}.");
        let inherited = self
            .report
            .obligations
            .iter()
            .filter(|obligation| obligation.path.starts_with(&child_prefix))
            .cloned()
            .collect::<Vec<_>>();
        for obligation in inherited {
            let suffix = obligation.path.trim_start_matches(&child_prefix);
            let path = format!("{parent_name}.{run_name}.{suffix}");
            if self
                .report
                .obligations
                .iter()
                .any(|existing| existing.path == path)
            {
                continue;
            }
            self.push_obligation(
                &parent_item.file_key,
                span.clone(),
                &obligation.kind,
                path,
                obligation.expected,
                format!(
                    "child run {} remains incomplete: {}",
                    run_name, obligation.message
                ),
                obligation.condition,
            );
        }
    }

    fn infer_expr(&mut self, file_key: &str, expression: &Expr, environment: &TypeEnv) -> Type {
        match &expression.kind {
            ExprKind::Presentation { value, unit, .. } => {
                let ty = self.infer_expr(file_key, value, environment);
                if ty == Type::Error {
                    return ty;
                }
                let Some(dimension) = ty.dimension() else {
                    self.push_error(
                        file_key,
                        expression.span.clone(),
                        "E0512",
                        format!("presentation requires a numeric value, found {ty}"),
                    );
                    return Type::Error;
                };
                let Some(unit_expression) = unit else {
                    return ty;
                };
                let label = unit_expression.display();
                let unit = match self.registry.resolve_display_unit(&label, unit_expression) {
                    Ok(unit) => unit,
                    Err(reason) => {
                        self.push_error(
                            file_key,
                            expression.span.clone(),
                            "E0511",
                            format!("invalid display unit {label}: {reason}"),
                        );
                        return Type::Error;
                    }
                };
                if dimension != unit.dimension {
                    self.push_error(file_key, expression.span.clone(), "E0512",
                        format!("value has dimension {dimension}, incompatible with display unit {label} ({})", unit.dimension));
                    return Type::Error;
                }
                let absolute = matches!(ty, Type::Quantity { absolute: true, .. });
                self.registry
                    .units
                    .entry(label.clone())
                    .or_insert_with(|| unit.clone());
                // A count remains an Integer/Scalar so it can still satisfy
                // scalar parameters and annotations. The AST retains its label.
                if dimension.is_scalar() {
                    return ty;
                }
                Type::Quantity {
                    dimension,
                    display: label,
                    scale: Some(unit.scale),
                    explicit_presentation: true,
                    absolute,
                }
            }
            ExprKind::Name { value } => {
                if let Some(ty) = environment.values.get(value).cloned() {
                    if environment.ownership.get(value) == Some(&PlaceState::Moved) {
                        self.push_error(
                            file_key,
                            expression.span.clone(),
                            "E0702",
                            format!("owned value {value} is used after it was moved"),
                        );
                    }
                    return ty;
                }
                (if value == "true" || value == "false" {
                    Some(Type::Bool)
                } else if let Some(unit) = self.registry.units.get(value) {
                    Some(if unit.dimension.is_scalar() {
                        Type::Scalar
                    } else {
                        Type::Quantity {
                            dimension: unit.dimension.clone(),
                            display: value.clone(),
                            scale: Some(unit.scale.clone()),
                            explicit_presentation: false,
                            absolute: unit.is_absolute(),
                        }
                    })
                } else if self.artifacts.contains_key(value) && self.is_visible(file_key, value) {
                    Some(Type::Artifact(value.clone()))
                } else if self.processes.contains_key(value) && self.is_visible(file_key, value) {
                    Some(Type::ConfiguredProcess(value.clone()))
                } else if self.functions.contains_key(value) && self.is_visible(file_key, value) {
                    Some(Type::Function)
                } else if value == "math" {
                    // A module namespace, not a value. Nothing is reported at
                    // this point, so this stays the strict `Unknown`: using
                    // `math` as a value is caught wherever it is consumed.
                    Some(Type::Unknown)
                } else {
                    None
                })
                .unwrap_or_else(|| {
                    self.push_error(
                        file_key,
                        expression.span.clone(),
                        "E0811",
                        format!("unknown name {value}"),
                    );
                    Type::Error
                })
            }
            ExprKind::Bool { .. } => Type::Bool,
            ExprKind::String { .. } => Type::String,
            ExprKind::Number { value, suffix } => {
                if let Some(suffix) = suffix {
                    if let Some(unit) = self.registry.units.get(suffix) {
                        if unit.dimension.is_scalar() {
                            Type::Scalar
                        } else {
                            Type::Quantity {
                                dimension: unit.dimension.clone(),
                                display: suffix.clone(),
                                scale: Some(unit.scale.clone()),
                                explicit_presentation: false,
                                absolute: unit.is_absolute(),
                            }
                        }
                    } else {
                        self.push_error(
                            file_key,
                            expression.span.clone(),
                            "E0801",
                            format!("unknown unit or dimension {suffix}"),
                        );
                        Type::Error
                    }
                } else if value.contains(['.', 'e', 'E']) {
                    Type::Scalar
                } else {
                    Type::Integer
                }
            }
            ExprKind::NamedTuple { fields } => {
                let mut types = Vec::new();
                for field in fields {
                    let ty = self.infer_expr(file_key, &field.value, environment);
                    if !type_is_copyable_in_env(&ty, environment) {
                        self.push_error(
                            file_key,
                            field.span.clone(),
                            "E0860",
                            format!(
                                "named tuple field {} must be copyable, found {ty}",
                                field.name.as_deref().unwrap_or("?")
                            ),
                        );
                    }
                    types.push((field.name.clone().expect("named tuple field"), ty));
                }
                Type::NamedTuple(types)
            }
            ExprKind::Array { items } => {
                // An empty array literal is genuinely polymorphic, so it seeds
                // with the permissive type and satisfies any element
                // annotation. A non-empty array overwrites this immediately.
                let mut element = Type::Error;
                for item in items {
                    let item_type = self.infer_expr(file_key, item, environment);
                    if element == Type::Error {
                        element = item_type;
                    } else if !compatible(&element, &item_type) {
                        self.push_error(
                            file_key,
                            item.span.clone(),
                            "E0802",
                            format!("array item has type {item_type}, expected {element}"),
                        );
                    }
                }
                Type::Array {
                    element: Box::new(element),
                    size: Some(items.len().to_string()),
                }
            }
            ExprKind::Unary { op, value } => {
                let value_type = self.infer_expr(file_key, value, environment);
                match op {
                    UnaryOp::Not => {
                        if value_type != Type::Bool && value_type != Type::Error {
                            self.push_error(
                                file_key,
                                expression.span.clone(),
                                "E0803",
                                format!("logical not expects Bool, found {value_type}"),
                            );
                        }
                        Type::Bool
                    }
                    // `-40 Celsius` is the literal -40 on the Celsius scale, not
                    // the negation of a point: sub-zero readings are ordinary
                    // temperatures. Only a sign applied to a *computed* absolute
                    // value is a category error, so `-room_temperature` still is.
                    UnaryOp::Neg
                        if value_type.is_absolute()
                            && !matches!(value.kind, ExprKind::Number { .. }) =>
                    {
                        self.reject_absolute(file_key, &expression.span, &value_type, "negate");
                        Type::Error
                    }
                    UnaryOp::Neg | UnaryOp::Pos => value_type,
                }
            }
            ExprKind::Binary { op, left, right } => {
                let left_type = self.infer_expr(file_key, left, environment);
                let right_type = self.infer_expr(file_key, right, environment);
                self.infer_binary(
                    file_key,
                    expression,
                    *op,
                    (left_type, right_type),
                    right,
                    environment,
                )
            }
            ExprKind::Member { value, member } => {
                if value.as_name() == Some("math") {
                    if !self.imports_module(file_key, "math") {
                        self.push_error(
                            file_key,
                            expression.span.clone(),
                            "E0817",
                            "math must be imported before use",
                        );
                        return Type::Error;
                    }
                    return if member == "pi" {
                        Type::Scalar
                    } else {
                        Type::Function
                    };
                }
                let base_type = self.infer_expr(file_key, value, environment);
                let resolved = match base_type.clone() {
                    Type::Artifact(name) => self
                        .model
                        .artifacts
                        .get(&name)
                        .and_then(|artifact| artifact.fields.get(member))
                        .cloned(),
                    Type::Run(process) => self
                        .resolved_members
                        .get(&process)
                        .and_then(|members| members.get(member))
                        .filter(|member| member.role != MemberRole::Config)
                        .map(|member| member.ty.clone()),
                    Type::NamedTuple(fields) => fields
                        .into_iter()
                        .find(|(name, _)| name == member)
                        .map(|(_, ty)| ty),
                    Type::Array { element, size } => match *element {
                        Type::Artifact(artifact_name) => self
                            .model
                            .artifacts
                            .get(&artifact_name)
                            .and_then(|artifact| artifact.fields.get(member))
                            .cloned()
                            .map(|field| Type::Array {
                                element: Box::new(field),
                                size,
                            })
                            .or_else(|| {
                                (member == "length" || member == "append").then_some(Type::Function)
                            }),
                        _ if member == "length" || member == "append" => Some(Type::Function),
                        _ => None,
                    },
                    Type::Stream(_) if member == "length" => Some(Type::Function),
                    Type::Error => return Type::Error,
                    _ => None,
                };
                if let Some(resolved) = resolved {
                    resolved
                } else {
                    self.push_error(
                        file_key,
                        expression.span.clone(),
                        "E0812",
                        format!("type {base_type} has no member {member}"),
                    );
                    Type::Error
                }
            }
            ExprKind::Index {
                value, wildcard, ..
            } => {
                if !*wildcard && let Some(resource) = self.resource_constructor(expression) {
                    let Type::Resource {
                        name,
                        capacity,
                        claim,
                    } = &resource
                    else {
                        unreachable!("resource constructor returns a resource type")
                    };
                    if !self.is_visible(file_key, name) {
                        self.push_error(
                            file_key,
                            expression.span.clone(),
                            "E0518",
                            format!("resource type {name} is not visible in this module"),
                        );
                        return Type::Error;
                    }
                    let claim_expression = claim
                        .as_deref()
                        .and_then(|claim| crate::parser::parse_expression(claim).ok());
                    if let Some(claim_expression) = claim_expression {
                        let claim_type = self.infer_expr(file_key, &claim_expression, environment);
                        if !compatible(capacity, &claim_type) {
                            self.push_error(
                                file_key,
                                expression.span.clone(),
                                "E0519",
                                format!(
                                    "resource constructor {name} expects {capacity}, found {claim_type}"
                                ),
                            );
                        }
                        let normalized =
                            normalize_expr(&claim_expression, &environment.expressions);
                        if numeric_constant(&normalized)
                            .is_some_and(|amount| amount <= BigRational::zero())
                        {
                            self.push_error(
                                file_key,
                                expression.span.clone(),
                                "E0519",
                                format!("resource constructor {name} requires positive capacity"),
                            );
                        }
                        if let (
                            Some((constructed, constructed_dimension)),
                            Some((declared, declared_dimension)),
                        ) = (
                            self.constant_quantity(&normalized),
                            self.declared_resource_amount(name),
                        ) && constructed_dimension == declared_dimension
                            && constructed > declared
                        {
                            self.push_error(
                                file_key,
                                expression.span.clone(),
                                "E0519",
                                format!(
                                    "resource constructor {name} exceeds declared capacity {}",
                                    format_exact_rational(&declared)
                                ),
                            );
                        }
                    }
                    return resource;
                }
                match self.infer_expr(file_key, value, environment) {
                    Type::Array { element, .. } => {
                        if *wildcard {
                            Type::Array {
                                element,
                                size: None,
                            }
                        } else {
                            *element
                        }
                    }
                    Type::Stream(element) => *element,
                    other => {
                        self.push_error(
                            file_key,
                            expression.span.clone(),
                            "E0804",
                            format!("cannot index {other}"),
                        );
                        Type::Error
                    }
                }
            }
            ExprKind::Lambda { .. } => Type::Function,
            ExprKind::Call { callee, arguments } => {
                let path = callee.path().unwrap_or_default();
                if path == "cardinality" {
                    if arguments.len() != 1 || arguments[0].name.is_some() {
                        self.push_error(
                            file_key,
                            expression.span.clone(),
                            "E0818",
                            "cardinality expects exactly one positional collection argument",
                        );
                        return Type::Error;
                    }
                    let collection = self.infer_expr(file_key, &arguments[0].value, environment);
                    return match collection {
                        Type::Array { .. } | Type::Stream(_) => Type::Integer,
                        Type::Error => Type::Error,
                        other => {
                            self.push_error(
                                file_key,
                                arguments[0].value.span.clone(),
                                "E0834",
                                format!("cardinality expects an array or stream, found {other}"),
                            );
                            Type::Error
                        }
                    };
                }
                if matches!(path.as_str(), "std.stream.zip" | "std.stream.zip_exact") {
                    if !self.imports_module(file_key, "std.stream") {
                        self.push_error(
                            file_key,
                            callee.span.clone(),
                            "E0817",
                            "std.stream must be imported before use",
                        );
                        return Type::Error;
                    }
                    if arguments.len() != 2 {
                        self.push_error(
                            file_key,
                            expression.span.clone(),
                            "E0818",
                            format!("{path} expects exactly two stream arguments"),
                        );
                        return Type::Error;
                    }
                    let mut fields = Vec::new();
                    let mut names = HashSet::new();
                    let mut stream_paths = HashSet::new();
                    for (index, argument) in arguments.iter().enumerate() {
                        let name = argument
                            .name
                            .clone()
                            .unwrap_or_else(|| ["left", "right"][index].into());
                        if !names.insert(name.clone()) {
                            self.push_error(
                                file_key,
                                argument.span.clone(),
                                "E0832",
                                format!("{path} output field {name} is duplicated"),
                            );
                        }
                        if let Some(stream_path) = argument.value.path()
                            && !stream_paths.insert(stream_path.clone())
                        {
                            self.push_error(
                                file_key,
                                argument.value.span.clone(),
                                "E0701",
                                format!(
                                    "owned stream {stream_path} cannot supply both inputs to {path}"
                                ),
                            );
                        }
                        let ty = self.infer_expr(file_key, &argument.value, environment);
                        let element = match ty {
                            Type::Stream(element) => *element,
                            Type::Error => Type::Error,
                            other => {
                                self.push_error(
                                    file_key,
                                    argument.value.span.clone(),
                                    "E0831",
                                    format!("{path} expects a stream, found {other}"),
                                );
                                Type::Error
                            }
                        };
                        fields.push((name, element));
                    }
                    return Type::Stream(Box::new(Type::NamedTuple(fields)));
                }
                if path == "is_integer" {
                    if arguments.len() != 1 || arguments[0].name.is_some() {
                        self.push_error(
                            file_key,
                            expression.span.clone(),
                            "E0818",
                            "is_integer expects one dimensionless numeric argument",
                        );
                        return Type::Error;
                    }
                    let ty = self.infer_expr(file_key, &arguments[0].value, environment);
                    if !matches!(ty, Type::Integer | Type::Scalar | Type::Error) {
                        self.push_error(
                            file_key,
                            expression.span.clone(),
                            "E0805",
                            "is_integer expects a dimensionless numeric value",
                        );
                        return Type::Error;
                    }
                    return Type::Bool;
                }
                if path.starts_with("math.") && !self.imports_module(file_key, "math") {
                    self.push_error(
                        file_key,
                        callee.span.clone(),
                        "E0817",
                        "math must be imported before use",
                    );
                    return Type::Error;
                }
                if path.starts_with("math.")
                    && arguments.iter().any(|argument| argument.name.is_some())
                {
                    self.push_error(
                        file_key,
                        expression.span.clone(),
                        "E0830",
                        format!("native function {path} accepts positional arguments only"),
                    );
                    return Type::Error;
                }
                if path == "math.sum" {
                    if arguments.len() != 1 {
                        self.push_error(
                            file_key,
                            expression.span.clone(),
                            "E0818",
                            "math.sum expects exactly one array argument",
                        );
                        return Type::Error;
                    }
                    let ty = self.infer_expr(file_key, &arguments[0].value, environment);
                    return match ty {
                        Type::Array { element, .. } if element.dimension().is_some() => *element,
                        _ => {
                            self.push_error(
                                file_key,
                                arguments[0].value.span.clone(),
                                "E0819",
                                format!("math.sum expects a numeric array, found {ty}"),
                            );
                            Type::Error
                        }
                    };
                }
                if path == "math.range" {
                    if arguments.len() != 2 {
                        self.push_error(
                            file_key,
                            expression.span.clone(),
                            "E0818",
                            "math.range expects exactly two Integer arguments",
                        );
                        return Type::Error;
                    }
                    for argument in arguments {
                        let ty = self.infer_expr(file_key, &argument.value, environment);
                        if ty != Type::Integer && ty != Type::Error {
                            self.push_error(
                                file_key,
                                argument.value.span.clone(),
                                "E0829",
                                format!("math.range bounds must be Integer, found {ty}"),
                            );
                        }
                    }
                    return Type::Array {
                        element: Box::new(Type::Integer),
                        size: None,
                    };
                }
                if path == "math.ceil" || path == "math.floor" {
                    if arguments.len() != 1 {
                        self.push_error(
                            file_key,
                            expression.span.clone(),
                            "E0818",
                            format!("{path} expects exactly one argument"),
                        );
                        return Type::Error;
                    }
                    if let Some(argument) = arguments.first() {
                        let ty = self.infer_expr(file_key, &argument.value, environment);
                        if ty.dimension().is_none() {
                            self.push_error(
                                file_key,
                                argument.value.span.clone(),
                                "E0805",
                                format!("{path} requires a numeric value, found {ty}"),
                            );
                            return Type::Error;
                        }
                        return match ty {
                            Type::Integer | Type::Scalar => Type::Integer,
                            quantity @ Type::Quantity { .. } => quantity,
                            _ => Type::Error,
                        };
                    }
                    return Type::Error;
                }
                if path == "math.sqrt" {
                    if arguments.len() != 1 {
                        self.push_error(
                            file_key,
                            expression.span.clone(),
                            "E0818",
                            "math.sqrt expects exactly one argument",
                        );
                        return Type::Error;
                    }
                    let Some(argument) = arguments.first() else {
                        return Type::Error;
                    };
                    let ty = self.infer_expr(file_key, &argument.value, environment);
                    return match ty {
                        Type::Integer | Type::Scalar => Type::Scalar,
                        Type::Quantity { absolute: true, .. } => {
                            self.reject_absolute(
                                file_key,
                                &argument.value.span,
                                &ty,
                                "take the square root of",
                            );
                            Type::Error
                        }
                        Type::Quantity {
                            dimension,
                            display,
                            scale,
                            explicit_presentation: _,
                            absolute: _,
                        } => Type::Quantity {
                            dimension: dimension.pow(&BigRational::new(1.into(), 2.into())),
                            display: format!("sqrt({display})"),
                            scale: scale.filter(BigRational::is_one),
                            explicit_presentation: false,
                            absolute: false,
                        },
                        _ => {
                            self.push_error(
                                file_key,
                                argument.value.span.clone(),
                                "E0822",
                                format!("math.sqrt expects a numeric value, found {ty}"),
                            );
                            Type::Error
                        }
                    };
                }
                if let ExprKind::Member { member, .. } = &callee.kind
                    && (member == "length" || member == "append")
                {
                    let callable = self.infer_expr(file_key, callee, environment);
                    if callable == Type::Error {
                        // The receiver already failed; propagate rather than
                        // add a second complaint about the call.
                        return Type::Error;
                    }
                    let expected = usize::from(member == "append");
                    if arguments.len() != expected {
                        self.push_error(
                            file_key,
                            expression.span.clone(),
                            "E0818",
                            format!("{member} expects {expected} argument(s)"),
                        );
                        return Type::Error;
                    }
                    return if member == "length" {
                        Type::Integer
                    } else {
                        Type::NamedTuple(Vec::new())
                    };
                }
                if path == "every" {
                    if arguments.len() != 2 {
                        self.push_error(
                            file_key,
                            expression.span.clone(),
                            "E0818",
                            "every expects an array and a predicate",
                        );
                        return Type::Error;
                    }
                    let collection = self.infer_expr(file_key, &arguments[0].value, environment);
                    if !matches!(collection, Type::Array { .. }) {
                        self.push_error(
                            file_key,
                            arguments[0].value.span.clone(),
                            "E0820",
                            format!("every expects an array, found {collection}"),
                        );
                    }
                    if !matches!(arguments[1].value.kind, ExprKind::Lambda { .. }) {
                        self.push_error(
                            file_key,
                            arguments[1].value.span.clone(),
                            "E0821",
                            "every expects a predicate lambda",
                        );
                    }
                    return Type::Bool;
                }
                if path == "if_then_else" {
                    if arguments.len() != 3 {
                        self.push_error(
                            file_key,
                            expression.span.clone(),
                            "E0818",
                            "if_then_else expects condition, then value, and else value",
                        );
                        return Type::Error;
                    }
                    return arguments
                        .get(1)
                        .map(|argument| self.infer_expr(file_key, &argument.value, environment))
                        .unwrap_or(Type::Error);
                }
                if path.is_empty() && matches!(&callee.kind, ExprKind::Call { .. }) {
                    return match self.infer_expr(file_key, callee, environment) {
                        Type::ConfiguredProcess(process) => Type::Run(process),
                        Type::Error => Type::Error,
                        other => {
                            self.push_error(
                                file_key,
                                callee.span.clone(),
                                "E0833",
                                format!("only a configured process can be invoked, found {other}"),
                            );
                            Type::Error
                        }
                    };
                }
                if let Some(artifact) = self
                    .model
                    .artifacts
                    .get(&path)
                    .filter(|_| self.is_visible(file_key, &path))
                    .cloned()
                {
                    let supplied = named_arguments(arguments);
                    for name in supplied.keys() {
                        if !artifact.fields.contains_key(name) {
                            self.push_error(
                                file_key,
                                expression.span.clone(),
                                "E0814",
                                format!("unknown artifact field {path}.{name}"),
                            );
                        }
                    }
                    for (name, expected) in artifact.fields {
                        if artifact.derived_fields.contains(&name) {
                            if supplied.contains_key(&name) {
                                self.push_error(
                                    file_key,
                                    expression.span.clone(),
                                    "E0806",
                                    format!(
                                        "derived artifact field {path}.{name} cannot be supplied"
                                    ),
                                );
                            }
                            continue;
                        }
                        if let Some(value) = supplied.get(&name) {
                            let actual = self.infer_expr(file_key, value, environment);
                            if !compatible(&expected, &actual) {
                                self.push_error(
                                    file_key,
                                    value.span.clone(),
                                    "E0807",
                                    format!("artifact field {path}.{name} expects {expected}, found {actual}"),
                                );
                            }
                        } else {
                            self.push_obligation(
                                file_key,
                                expression.span.clone(),
                                "missing_artifact_field",
                                format!("{path}.{name}"),
                                expected.to_string(),
                                format!("artifact construction omits field {name}"),
                                None,
                            );
                        }
                    }
                    return Type::Artifact(path);
                }
                if self.processes.contains_key(&path) && self.is_visible(file_key, &path) {
                    return Type::ConfiguredProcess(path);
                }
                if let Some(Type::ConfiguredProcess(process)) = callee
                    .as_name()
                    .and_then(|name| environment.values.get(name))
                    .cloned()
                {
                    return Type::Run(process);
                }
                if let Some(function) = self
                    .model
                    .functions
                    .get(&path)
                    .filter(|_| self.is_visible(file_key, &path))
                    .cloned()
                {
                    let parameter_names = function.parameters.keys().cloned().collect::<Vec<_>>();
                    let mut supplied = HashSet::new();
                    let mut substitutions = IndexMap::<String, Type>::new();
                    for (index, argument) in arguments.iter().enumerate() {
                        let parameter_name = argument
                            .name
                            .clone()
                            .or_else(|| parameter_names.get(index).cloned());
                        let Some(parameter_name) = parameter_name else {
                            self.push_error(
                                file_key,
                                argument.span.clone(),
                                "E0823",
                                format!("function {path} received too many arguments"),
                            );
                            continue;
                        };
                        let Some(expected) = function.parameters.get(&parameter_name) else {
                            self.push_error(
                                file_key,
                                argument.span.clone(),
                                "E0824",
                                format!("function {path} has no parameter {parameter_name}"),
                            );
                            continue;
                        };
                        if !supplied.insert(parameter_name.clone()) {
                            self.push_error(
                                file_key,
                                argument.span.clone(),
                                "E0825",
                                format!(
                                    "function parameter {parameter_name} is supplied more than once"
                                ),
                            );
                            continue;
                        }
                        let actual = self.infer_expr(file_key, &argument.value, environment);
                        if !match_generic_type(expected, &actual, &mut substitutions) {
                            self.push_error(
                                file_key,
                                argument.value.span.clone(),
                                "E0826",
                                format!("function parameter {parameter_name} expects {expected}, found {actual}"),
                            );
                        }
                    }
                    for (name, expected) in &function.parameters {
                        if !supplied.contains(name) {
                            self.push_error(
                                file_key,
                                expression.span.clone(),
                                "E0827",
                                format!("function call {path} omits parameter {name}: {expected}"),
                            );
                        }
                    }
                    for bound in &function.bounds {
                        if let Some((parameter, capability)) = bound.split_once(": ")
                            && let Some(actual) = substitutions.get(parameter)
                            && !self.type_satisfies_bound(actual, capability, environment)
                        {
                            self.push_error(
                                file_key,
                                expression.span.clone(),
                                "E0828",
                                format!(
                                    "type {actual} does not satisfy generic bound {bound} for function {path}"
                                ),
                            );
                        }
                    }
                    return Type::NamedTuple(
                        function
                            .outputs
                            .iter()
                            .map(|(name, ty)| {
                                (name.clone(), substitute_generic_type(ty, &substitutions))
                            })
                            .collect(),
                    );
                }
                self.push_error(
                    file_key,
                    expression.span.clone(),
                    "E0813",
                    format!("unknown function, constructor, or process call {path}"),
                );
                Type::Error
            }
        }
    }

    fn type_satisfies_bound(&self, ty: &Type, capability: &str, environment: &TypeEnv) -> bool {
        if CORE_CAPABILITIES.contains(&capability) {
            return type_has_capability(ty, capability, environment);
        }
        let artifact_names = self.artifact_name_map();
        self.items.iter().any(|item| {
            let Item::Impl(value) = &item.item else {
                return false;
            };
            if value.trait_name != capability || value.arguments.len() != 1 {
                return false;
            }
            let implemented = self
                .registry
                .resolve_type(&value.arguments[0], &artifact_names, &[]);
            coherence_type_key(&implemented) == coherence_type_key(ty)
        })
    }

    /// The interval unit that measures differences on an absolute quantity's
    /// scale, as a relative [`Type::Quantity`]. `Celsius` yields `Kelvin`,
    /// `Fahrenheit` yields `Rankine`.
    fn relative_counterpart_type(&self, ty: &Type) -> Option<Type> {
        let unit = self
            .registry
            .units
            .get(ty.display_unit_name()?)
            .filter(|unit| unit.is_absolute())?;
        let relative = self.registry.relative_counterpart(unit)?;
        Some(Type::Quantity {
            dimension: relative.dimension.clone(),
            display: relative.name.clone(),
            scale: Some(relative.scale.clone()),
            explicit_presentation: false,
            absolute: false,
        })
    }

    /// The identity element for an aggregate over `ty`.
    ///
    /// Never emits an absolute unit: `0 Celsius` is 273.15 K, not nothing, so
    /// seeding a total with it would silently shift every aggregate. Absolute
    /// exports cannot be summed at all (see `E0509`), but the zero still has to
    /// be well formed for the paths that build one before that check bites.
    fn zero_expression(&self, ty: &Type, span: Span) -> Expr {
        let suffix = match ty {
            Type::Quantity { display, .. } if ty.is_absolute() => self
                .registry
                .units
                .get(display.as_str())
                .and_then(|unit| self.registry.relative_counterpart(unit))
                .map(|relative| relative.name.clone()),
            Type::Quantity { display, .. } => Some(display.clone()),
            _ => None,
        };
        Expr {
            kind: ExprKind::Number {
                value: "0".into(),
                suffix,
            },
            span,
        }
    }

    /// Report an absolute (affine point) quantity used where a magnitude is
    /// required, pointing at the interval unit that would work instead.
    fn reject_absolute(&mut self, file_key: &str, span: &Span, ty: &Type, action: &str) {
        let suggestion = ty
            .display_unit_name()
            .and_then(|name| self.registry.units.get(name))
            .and_then(|unit| self.registry.relative_counterpart(unit))
            .map_or_else(
                || "a relative unit".to_owned(),
                |relative| relative.name.clone(),
            );
        self.push_error(
            file_key,
            span.clone(),
            "E0836",
            format!(
                "cannot {action} the absolute quantity {ty}: it measures a point on a scale, not \
                 a magnitude. Use {suggestion} for temperature differences, rates, and per-degree \
                 quantities"
            ),
        );
    }

    /// Two absolute quantities of the same dimension compare fine — their
    /// canonical values are directly ordered. Comparing a point against a
    /// magnitude does not typecheck: `room > 5 Kelvin` is a category error even
    /// though both sides are temperatures.
    fn check_comparable_absoluteness(
        &mut self,
        file_key: &str,
        expression: &Expr,
        left: &Type,
        right: &Type,
    ) {
        if left.is_absolute() == right.is_absolute() {
            return;
        }
        let (absolute, relative) = if left.is_absolute() {
            (left, right)
        } else {
            (right, left)
        };
        self.push_error(
            file_key,
            expression.span.clone(),
            "E0836",
            format!(
                "cannot compare the absolute quantity {absolute} with the magnitude {relative}: \
                 one is a point on a scale and the other is a difference"
            ),
        );
    }

    fn infer_binary(
        &mut self,
        file_key: &str,
        expression: &Expr,
        op: BinaryOp,
        operands: (Type, Type),
        right_expression: &Expr,
        environment: &TypeEnv,
    ) -> Type {
        let (left, right) = operands;
        // An invalid operand already has a diagnostic at its source. Avoid
        // misleading cascades such as multiplication by `Unknown` after a
        // rejected exponent.
        if matches!(left, Type::Error) || matches!(right, Type::Error) {
            return Type::Error;
        }
        // Exhaustive by construction: adding a `BinaryOp` variant is a compile
        // error here rather than a silent fall-through to an unchecked type.
        match op {
            BinaryOp::And | BinaryOp::Or => {
                let operand_is_bool = |ty: &Type| matches!(ty, Type::Bool | Type::Error);
                if !operand_is_bool(&left) || !operand_is_bool(&right) {
                    self.push_error(
                        file_key,
                        expression.span.clone(),
                        "E0835",
                        format!("logical {op} expects Bool operands, found {left} and {right}"),
                    );
                }
                Type::Bool
            }
            BinaryOp::Eq | BinaryOp::NotEq => {
                if !compatible(&left, &right)
                    || !type_has_capability(&left, "Equatable", environment)
                    || !type_has_capability(&right, "Equatable", environment)
                {
                    self.push_error(
                        file_key,
                        expression.span.clone(),
                        "E0808",
                        format!("cannot compare {left} with {right}"),
                    );
                } else {
                    self.check_comparable_absoluteness(file_key, expression, &left, &right);
                }
                Type::Bool
            }
            BinaryOp::Lt | BinaryOp::Le | BinaryOp::Gt | BinaryOp::Ge => {
                if !compatible(&left, &right)
                    || !type_has_capability(&left, "Ordered", environment)
                    || !type_has_capability(&right, "Ordered", environment)
                {
                    self.push_error(
                        file_key,
                        expression.span.clone(),
                        "E0808",
                        format!("cannot order {left} and {right}"),
                    );
                } else {
                    self.check_comparable_absoluteness(file_key, expression, &left, &right);
                }
                Type::Bool
            }
            BinaryOp::Add | BinaryOp::Sub => {
                let capability = if op == BinaryOp::Add {
                    "Add"
                } else {
                    "Subtract"
                };
                if compatible(&left, &right)
                    && type_has_capability(&left, capability, environment)
                    && matches!(left, Type::TypeParameter(_))
                {
                    return left;
                }
                if left.dimension().is_none() || right.dimension().is_none() {
                    self.push_error(
                        file_key,
                        expression.span.clone(),
                        "E0809",
                        format!("cannot apply {op} to {left} and {right}"),
                    );
                    return Type::Error;
                }
                if !compatible(&left, &right) {
                    self.push_error(
                        file_key,
                        expression.span.clone(),
                        "E0809",
                        format!("cannot apply {op} to {left} and {right}"),
                    );
                    return Type::Error;
                }
                match (left.is_absolute(), right.is_absolute()) {
                    // A difference of two points is a magnitude: `25 Celsius -
                    // 20 Celsius` is 5 Kelvin, not 5 Celsius.
                    (true, true) if op == BinaryOp::Sub => {
                        return self
                            .relative_counterpart_type(&left)
                            .unwrap_or_else(|| left.clone());
                    }
                    (true, true) => {
                        self.push_error(
                            file_key,
                            expression.span.clone(),
                            "E0837",
                            format!(
                                "cannot add two absolute temperatures ({left} and {right}); a \
                                 point plus a point has no meaning. Use a temperature difference \
                                 in {} for the offset",
                                self.relative_counterpart_type(&left)
                                    .map_or_else(|| "Kelvin".to_owned(), |ty| ty.to_string())
                            ),
                        );
                        return Type::Error;
                    }
                    // `20 Celsius + 5 Kelvin` and `20 Celsius - 5 Kelvin` both
                    // land back on the absolute scale.
                    (true, false) => return left,
                    // `5 Kelvin + 20 Celsius` is the same displacement written
                    // the other way round; `5 Kelvin - 20 Celsius` is not.
                    (false, true) if op == BinaryOp::Add => return right,
                    (false, true) => {
                        self.reject_absolute(
                            file_key,
                            &expression.span,
                            &right,
                            "subtract from a magnitude",
                        );
                        return Type::Error;
                    }
                    (false, false) => {}
                }
                if left == Type::Integer && right == Type::Scalar {
                    Type::Scalar
                } else {
                    left
                }
            }
            BinaryOp::Mul | BinaryOp::Div => {
                let capability = if op == BinaryOp::Mul {
                    "Multiply"
                } else {
                    "Divide"
                };
                if type_has_capability(&left, capability, environment)
                    && matches!(left, Type::TypeParameter(_))
                {
                    return left;
                }
                for operand in [&left, &right] {
                    if operand.is_absolute() {
                        let action = if op == BinaryOp::Mul {
                            "multiply"
                        } else {
                            "divide"
                        };
                        self.reject_absolute(file_key, &expression.span, operand, action);
                        return Type::Error;
                    }
                }
                let Some(left_dimension) = left.dimension() else {
                    self.push_error(
                        file_key,
                        expression.span.clone(),
                        "E0815",
                        format!("cannot apply {op} to {left} and {right}"),
                    );
                    return Type::Error;
                };
                let Some(right_dimension) = right.dimension() else {
                    self.push_error(
                        file_key,
                        expression.span.clone(),
                        "E0815",
                        format!("cannot apply {op} to {left} and {right}"),
                    );
                    return Type::Error;
                };
                let dimension = if op == BinaryOp::Mul {
                    left_dimension.multiply(&right_dimension)
                } else {
                    left_dimension.divide(&right_dimension)
                };
                if op == BinaryOp::Mul && left_dimension.is_scalar() && !right_dimension.is_scalar()
                {
                    return right;
                }
                if right_dimension.is_scalar() && !left_dimension.is_scalar() {
                    return left;
                }
                let scale = match (left.unit_scale(), right.unit_scale()) {
                    (Some(left), Some(right)) if op == BinaryOp::Mul => Some(left * right),
                    (Some(left), Some(right)) => Some(left / right),
                    _ => None,
                };
                type_from_dimension(dimension, format!("({left} {op} {right})"), scale)
            }
            BinaryOp::Pow => {
                if type_has_capability(&left, "Power", environment)
                    && matches!(left, Type::TypeParameter(_))
                {
                    return left;
                }
                if left.is_absolute() {
                    self.reject_absolute(file_key, &expression.span, &left, "raise to a power");
                    return Type::Error;
                }
                let Some(dimension) = left.dimension() else {
                    self.push_error(
                        file_key,
                        expression.span.clone(),
                        "E0816",
                        format!("cannot raise {left} to a power"),
                    );
                    return Type::Error;
                };
                let exponent = signed_scalar_literal(right_expression).or_else(|| {
                    if matches!(right_expression.kind, ExprKind::Presentation { .. }) {
                        self.constant_quantity(right_expression)
                            .filter(|(_, dimension)| dimension.is_scalar())
                            .map(|(value, _)| value)
                    } else {
                        None
                    }
                });
                let Some(exponent) = exponent else {
                    let message = if right.dimension().is_some_and(|d| !d.is_scalar()) {
                        format!(
                            "exponent must be dimensionless, found {right}; a unit after the exponent belongs to the exponent. Use `as Unit` to convert the result, or `* Unit` outside the power to multiply by a unit"
                        )
                    } else {
                        "exponent must be a dimensionless numeric literal (optionally signed)"
                            .into()
                    };
                    self.push_error(file_key, right_expression.span.clone(), "E0810", message);
                    return Type::Error;
                };
                if exponent < BigRational::zero()
                    && let ExprKind::Binary { left: base, .. } = &expression.kind
                    && self
                        .constant_quantity(base)
                        .is_some_and(|(value, _)| value.is_zero())
                {
                    self.push_error(
                        file_key,
                        expression.span.clone(),
                        "E0816",
                        "zero cannot be raised to a negative power",
                    );
                    return Type::Error;
                }
                let scale = if exponent.denom().is_one() {
                    exponent
                        .numer()
                        .to_i32()
                        .and_then(|exponent| left.unit_scale().map(|scale| scale.pow(exponent)))
                } else {
                    None
                };
                type_from_dimension(
                    dimension.pow(&exponent),
                    format!("{left}^{exponent}"),
                    scale,
                )
            }
        }
    }

    fn evaluate_predicate(&self, expression: &Expr) -> Option<bool> {
        match &expression.kind {
            ExprKind::Call { callee, arguments }
                if callee.as_name() == Some("every") && arguments.len() == 2 =>
            {
                let ExprKind::Lambda { parameters, body } = &arguments[1].value.kind else {
                    return None;
                };
                if parameters.len() != 1 {
                    return None;
                }
                let ExprKind::Array { items } = &arguments[0].value.kind else {
                    return iterable_requirement_truth(&arguments[0].value, &parameters[0], body);
                };
                let mut all_known = true;
                for item in items {
                    let predicate = normalize_expr(
                        body,
                        &IndexMap::from([(parameters[0].clone(), item.clone())]),
                    );
                    match self.evaluate_predicate(&predicate) {
                        Some(false) => return Some(false),
                        Some(true) => {}
                        None => all_known = false,
                    }
                }
                all_known.then_some(true)
            }
            ExprKind::Call { callee, arguments }
                if callee.as_name() == Some("is_integer") && arguments.len() == 1 =>
            {
                let (value, dimension) = self.constant_quantity(&arguments[0].value)?;
                dimension.is_scalar().then(|| value.is_integer())
            }
            ExprKind::Member { value, member } => {
                self.evaluate_predicate(&self.tuple_member_value(value, member, 0)?)
            }
            ExprKind::Bool { value } => Some(*value),
            ExprKind::Unary {
                op: UnaryOp::Not,
                value,
            } => self.evaluate_predicate(value).map(|value| !value),
            // Short-circuiting keeps a decidable operand authoritative even
            // when the other side is symbolic: `false and x` is false.
            ExprKind::Binary {
                op: BinaryOp::And,
                left,
                right,
            } => match (
                self.evaluate_predicate(left),
                self.evaluate_predicate(right),
            ) {
                (Some(false), _) | (_, Some(false)) => Some(false),
                (Some(true), Some(true)) => Some(true),
                _ => None,
            },
            ExprKind::Binary {
                op: BinaryOp::Or,
                left,
                right,
            } => match (
                self.evaluate_predicate(left),
                self.evaluate_predicate(right),
            ) {
                (Some(true), _) | (_, Some(true)) => Some(true),
                (Some(false), Some(false)) => Some(false),
                _ => None,
            },
            // Text literals have no dimension, so they fold separately.
            ExprKind::Binary { op, left, right }
                if matches!(op, BinaryOp::Eq | BinaryOp::NotEq)
                    && matches!(left.kind, ExprKind::String { .. })
                    && matches!(right.kind, ExprKind::String { .. }) =>
            {
                let (ExprKind::String { value: left }, ExprKind::String { value: right }) =
                    (&left.kind, &right.kind)
                else {
                    return None;
                };
                Some(if *op == BinaryOp::Eq {
                    left == right
                } else {
                    left != right
                })
            }
            ExprKind::Binary { op, left, right } if op.is_comparison() => {
                let (left_value, left_dimension) = self.constant_quantity(left)?;
                let (right_value, right_dimension) = self.constant_quantity(right)?;
                if left_dimension != right_dimension {
                    return None;
                }
                match op {
                    BinaryOp::Eq => Some(left_value == right_value),
                    BinaryOp::NotEq => Some(left_value != right_value),
                    BinaryOp::Lt => Some(left_value < right_value),
                    BinaryOp::Le => Some(left_value <= right_value),
                    BinaryOp::Gt => Some(left_value > right_value),
                    BinaryOp::Ge => Some(left_value >= right_value),
                    BinaryOp::And
                    | BinaryOp::Or
                    | BinaryOp::Add
                    | BinaryOp::Sub
                    | BinaryOp::Mul
                    | BinaryOp::Div
                    | BinaryOp::Pow => None,
                }
            }
            _ => None,
        }
    }

    /// The unit a value should actually be printed in, given the unit that was
    /// asked for.
    ///
    /// A magnitude is never rendered in an absolute unit: `25 Celsius - 20
    /// Celsius` is a five-degree difference, and printing it as Celsius would
    /// read as -268.15. The relative counterpart is substituted instead.
    fn rendering_unit(&self, name: &str, expression: &Expr) -> Option<&Unit> {
        let unit = self.registry.units.get(name)?;
        if unit.is_absolute() && !expression_is_absolute(expression, &self.registry) {
            return self.registry.relative_counterpart(unit);
        }
        Some(unit)
    }

    fn metric_value_display(
        &self,
        expression: &Expr,
        ty: &Type,
        display_unit: Option<&str>,
    ) -> String {
        if display_unit.is_none()
            && let ExprKind::Presentation {
                value,
                unit: Some(unit),
                ..
            } = &expression.kind
        {
            return self.metric_value_display(value, ty, Some(&unit.display()));
        }
        let Some((value, dimension)) = self.constant_quantity(expression) else {
            if let ExprKind::Call { callee, arguments } = &expression.kind
                && let Some(name) = callee.path()
                && matches!(name.as_str(), "sum" | "makespan" | "max" | "min")
                && arguments.iter().all(|argument| argument.name.is_none())
            {
                let mut values = Vec::new();
                collect_flattened_aggregate_values(expression, &name, &mut values);
                if name == "sum" && values.len() > 1 {
                    let nonzero = values
                        .iter()
                        .copied()
                        .filter(|value| {
                            !self
                                .constant_quantity(value)
                                .is_some_and(|(value, _)| value.is_zero())
                        })
                        .collect::<Vec<_>>();
                    if !nonzero.is_empty() {
                        values = nonzero;
                    }
                }
                return format!(
                    "{name}({})",
                    values
                        .into_iter()
                        .map(|value| self.metric_value_display(value, ty, display_unit))
                        .collect::<Vec<_>>()
                        .join(", ")
                );
            }
            return expression.display();
        };
        if let Some(display_unit) = display_unit
            && let Some(unit) = self.rendering_unit(display_unit, expression)
            && unit.dimension == dimension
        {
            // Full affine inverse, so `42 Celsius as Fahrenheit` is exactly
            // 107.6 rather than a rounded multiplicative approximation.
            return format!(
                "{} {}",
                format_exact_rational(&unit.from_canonical(&value)),
                unit.name
            );
        }
        match ty {
            Type::Integer | Type::Scalar if dimension.is_scalar() => format_exact_rational(&value),
            Type::Quantity {
                dimension: expected,
                display,
                scale,
                explicit_presentation,
                absolute,
            } if *expected == dimension => {
                // An absolute quantity always displays in a single registered
                // unit, so its offset is recoverable from the registry.
                if *absolute && let Some(unit) = self.rendering_unit(display, expression) {
                    return format!(
                        "{} {}",
                        format_exact_rational(&unit.from_canonical(&value)),
                        unit.name
                    );
                }
                if *explicit_presentation
                    && let Some(scale) = scale
                    && !scale.is_zero()
                {
                    return format!("{} {display}", format_exact_rational(&(value / scale)));
                }
                let output_display = if quantity_suffixes_match(expression, display) {
                    Some(display.as_str())
                } else {
                    first_quantity_suffix(expression, &self.registry, &dimension)
                };
                if let Some(output_display) = output_display {
                    return self
                        .rendering_unit(output_display, expression)
                        .filter(|unit| unit.dimension == dimension)
                        .map(|unit| {
                            format!(
                                "{} {}",
                                format_exact_rational(&unit.from_canonical(&value)),
                                unit.name
                            )
                        })
                        .unwrap_or_else(|| expression.display());
                }
                if let Some((scale, label)) =
                    preferred_quantity_presentation(expression, &self.registry)
                    && scale != BigRational::zero()
                {
                    return format!("{} {label}", format_exact_rational(&(value / scale)));
                }
                expression.display()
            }
            _ => expression.display(),
        }
    }

    fn time_type(&self) -> Type {
        let unit = self
            .registry
            .units
            .get("Time")
            .expect("the built-in Time dimension is always registered");
        Type::Quantity {
            dimension: unit.dimension.clone(),
            display: "Time".into(),
            scale: Some(unit.scale.clone()),
            explicit_presentation: false,
            absolute: false,
        }
    }

    fn metric_bounds_display(
        &self,
        expression: &Expr,
        ty: &Type,
        display_unit: Option<&str>,
        reason: &str,
    ) -> Option<MetricBounds> {
        let bounds = self.quantity_bounds(expression)?;
        if !bounds.unresolved_makespan || bounds.lower == bounds.upper {
            return None;
        }

        let unit_name = display_unit
            .filter(|name| {
                self.registry
                    .units
                    .get(*name)
                    .is_some_and(|unit| unit.dimension == bounds.dimension)
            })
            .map(str::to_owned)
            .or(bounds.preferred_unit)
            .or_else(|| match ty {
                Type::Quantity {
                    dimension, display, ..
                } if *dimension == bounds.dimension
                    && self
                        .registry
                        .units
                        .get(display)
                        .is_some_and(|unit| unit.dimension == bounds.dimension) =>
                {
                    Some(display.clone())
                }
                _ => None,
            });

        let display_bound = |value: &BigRational| {
            if let Some(unit_name) = unit_name.as_deref()
                && let Some(unit) = self.registry.units.get(unit_name)
            {
                return format!(
                    "{} {unit_name}",
                    format_exact_rational(&unit.from_canonical(value))
                );
            }
            format_exact_rational(value)
        };

        Some(MetricBounds {
            lower: display_bound(&bounds.lower),
            upper: display_bound(&bounds.upper),
            reason: reason.to_owned(),
        })
    }

    fn quantity_bounds(&self, expression: &Expr) -> Option<QuantityBounds> {
        if let Some((value, dimension)) = self.constant_quantity(expression) {
            return Some(QuantityBounds {
                lower: value.clone(),
                upper: value,
                preferred_unit: first_quantity_suffix(expression, &self.registry, &dimension)
                    .map(str::to_owned),
                dimension,
                unresolved_makespan: false,
            });
        }

        let ExprKind::Call { callee, arguments } = &expression.kind else {
            return None;
        };
        let aggregate = callee.path()?;
        if !matches!(aggregate.as_str(), "sum" | "min" | "max" | "makespan")
            || arguments.is_empty()
            || arguments.iter().any(|argument| argument.name.is_some())
        {
            return None;
        }
        let values = arguments
            .iter()
            .map(|argument| self.quantity_bounds(&argument.value))
            .collect::<Option<Vec<_>>>()?;
        let dimension = values.first()?.dimension.clone();
        if values.iter().any(|value| value.dimension != dimension) {
            return None;
        }

        let unresolved_makespan =
            aggregate == "makespan" || values.iter().any(|value| value.unresolved_makespan);
        let (lower, upper, preferred_unit) = match aggregate.as_str() {
            "sum" => {
                let lower = values
                    .iter()
                    .fold(BigRational::zero(), |total, value| total + &value.lower);
                let upper = values
                    .iter()
                    .fold(BigRational::zero(), |total, value| total + &value.upper);
                let preferred = values
                    .iter()
                    .find(|value| value.unresolved_makespan && value.preferred_unit.is_some())
                    .or_else(|| values.iter().find(|value| value.preferred_unit.is_some()))
                    .and_then(|value| value.preferred_unit.clone());
                (lower, upper, preferred)
            }
            "min" => {
                let lower_value = values.iter().min_by(|left, right| {
                    left.lower
                        .partial_cmp(&right.lower)
                        .unwrap_or(std::cmp::Ordering::Equal)
                })?;
                let upper = values.iter().map(|value| value.upper.clone()).min()?;
                (
                    lower_value.lower.clone(),
                    upper,
                    lower_value.preferred_unit.clone(),
                )
            }
            "max" => {
                let lower_value = values.iter().max_by(|left, right| {
                    left.lower
                        .partial_cmp(&right.lower)
                        .unwrap_or(std::cmp::Ordering::Equal)
                })?;
                let upper = values.iter().map(|value| value.upper.clone()).max()?;
                (
                    lower_value.lower.clone(),
                    upper,
                    lower_value.preferred_unit.clone(),
                )
            }
            "makespan" => {
                if values.iter().any(|value| {
                    value.lower < BigRational::zero() || value.upper < BigRational::zero()
                }) {
                    return None;
                }
                let lower_value = values.iter().max_by(|left, right| {
                    left.lower
                        .partial_cmp(&right.lower)
                        .unwrap_or(std::cmp::Ordering::Equal)
                })?;
                let upper = values
                    .iter()
                    .fold(BigRational::zero(), |total, value| total + &value.upper);
                (
                    lower_value.lower.clone(),
                    upper,
                    lower_value.preferred_unit.clone(),
                )
            }
            _ => unreachable!(),
        };

        Some(QuantityBounds {
            lower,
            upper,
            dimension,
            preferred_unit,
            unresolved_makespan,
        })
    }

    /// The unit `ceil`/`floor` should round in: the author's own spelling when
    /// the argument names one, otherwise the canonical scale.
    ///
    /// Returns a whole [`Unit`] rather than a bare scale so that rounding an
    /// affine quantity rounds on its own scale — `ceil(20.5 Celsius)` is
    /// 21 Celsius, not 294 Kelvin.
    fn rounding_unit(&self, expression: &Expr, dimension: &Dimension) -> Unit {
        let named = match &expression.kind {
            ExprKind::Presentation {
                unit: Some(unit), ..
            } => self.registry.units.get(&unit.display()),
            ExprKind::Name { value } => self.registry.units.get(value),
            ExprKind::Number {
                suffix: Some(suffix),
                ..
            } => self.registry.units.get(suffix),
            _ => None,
        };
        if let Some(unit) = named.filter(|unit| unit.is_absolute() && unit.dimension == *dimension)
        {
            return unit.clone();
        }
        // Pure helper results can hide the author's concrete unit spelling. In
        // that case the quantity is already in the canonical scale used by
        // exact evaluation.
        let scale = preferred_unit(expression, &self.registry)
            .filter(|(_, resolved_dimension)| resolved_dimension == dimension)
            .map_or_else(BigRational::one, |(scale, _)| scale);
        Unit {
            name: String::new(),
            dimension: dimension.clone(),
            scale,
            offset: BigRational::zero(),
            composition: BTreeMap::new(),
            is_base_dimension: false,
        }
    }

    /// Project a structural value through tuple literals and named function
    /// results. Preserve unresolved projections rather than inventing values.
    fn tuple_member_value(&self, value: &Expr, member: &str, depth: usize) -> Option<Expr> {
        if depth > 64 {
            return None;
        }
        match &value.kind {
            ExprKind::NamedTuple { fields } => fields
                .iter()
                .find(|field| field.name.as_deref() == Some(member))
                .map(|field| field.value.clone()),
            ExprKind::Call { callee, arguments } => {
                self.inline_pure_function_output(&callee.path()?, arguments, member)
            }
            ExprKind::Member {
                value,
                member: inner,
            } => {
                let projected = self.tuple_member_value(value, inner, depth + 1)?;
                self.tuple_member_value(&projected, member, depth + 1)
            }
            _ => None,
        }
    }

    fn constant_quantity(&self, expression: &Expr) -> Option<(BigRational, Dimension)> {
        match &expression.kind {
            ExprKind::Presentation { value, .. } => self.constant_quantity(value),
            ExprKind::Member { value, member } => {
                let inlined = self.tuple_member_value(value, member, 0)?;
                self.constant_quantity(&inlined)
            }
            // Every constant folds to its canonical value, so an affine unit
            // has to contribute its offset here too: `20 Celsius` is 293.15 K.
            ExprKind::Name { value } => self.registry.units.get(value).map(|unit| {
                (
                    unit.to_canonical(&BigRational::one()),
                    unit.dimension.clone(),
                )
            }),
            ExprKind::Number { value, suffix } => {
                let value = parse_rational(value)?;
                match suffix {
                    Some(suffix) => self
                        .registry
                        .units
                        .get(suffix)
                        .map(|unit| (unit.to_canonical(&value), unit.dimension.clone())),
                    None => Some((value, Dimension::scalar())),
                }
            }
            ExprKind::Unary {
                op: UnaryOp::Neg,
                value,
            } => {
                // The sign belongs to the literal, which matters for affine
                // units: `-40 Celsius` is 233.15 K, not the negation of the
                // canonical value of `40 Celsius` (which would be -313.15 K).
                if let Some(literal) = negated_number_literal(value) {
                    return self.constant_quantity(&literal);
                }
                self.constant_quantity(value)
                    .map(|(value, dimension)| (-value, dimension))
            }
            ExprKind::Unary {
                op: UnaryOp::Pos,
                value,
            } => self.constant_quantity(value),
            ExprKind::Binary { op, left, right } if matches!(op, BinaryOp::Add | BinaryOp::Sub) => {
                let (left, left_dimension) = self.constant_quantity(left)?;
                let (right, right_dimension) = self.constant_quantity(right)?;
                if left_dimension != right_dimension {
                    return None;
                }
                Some((
                    if *op == BinaryOp::Add {
                        left + right
                    } else {
                        left - right
                    },
                    left_dimension,
                ))
            }
            ExprKind::Binary { op, left, right } if matches!(op, BinaryOp::Mul | BinaryOp::Div) => {
                let (left, left_dimension) = self.constant_quantity(left)?;
                let (right, right_dimension) = self.constant_quantity(right)?;
                if right.is_zero() && *op == BinaryOp::Div {
                    return None;
                }
                Some(if *op == BinaryOp::Mul {
                    (left * right, left_dimension.multiply(&right_dimension))
                } else {
                    (left / right, left_dimension.divide(&right_dimension))
                })
            }
            ExprKind::Binary {
                op: BinaryOp::Pow,
                left,
                right,
            } => {
                let (base, base_dimension) = self.constant_quantity(left)?;
                let (exponent, exponent_dimension) = self.constant_quantity(right)?;
                if !exponent_dimension.is_scalar() || !exponent.denom().is_one() {
                    return None;
                }
                let exponent_integer = exponent.numer().to_i32()?;
                if base.is_zero() && exponent_integer < 0 {
                    return None;
                }
                Some((base.pow(exponent_integer), base_dimension.pow(&exponent)))
            }
            ExprKind::Call { callee, arguments }
                if matches!(callee.path().as_deref(), Some("ceil" | "math.ceil"))
                    && arguments.len() == 1 =>
            {
                let (value, dimension) = self.constant_quantity(&arguments[0].value)?;
                let rounding = self.rounding_unit(&arguments[0].value, &dimension);
                let scaled = rounding.from_canonical(&value);
                let quotient = scaled.numer() / scaled.denom();
                let remainder = scaled.numer() % scaled.denom();
                let rounded = if remainder > BigInt::zero() {
                    quotient + BigInt::one()
                } else {
                    quotient
                };
                Some((
                    rounding.to_canonical(&BigRational::from_integer(rounded)),
                    dimension,
                ))
            }
            ExprKind::Call { callee, arguments }
                if matches!(callee.path().as_deref(), Some("floor" | "math.floor"))
                    && arguments.len() == 1 =>
            {
                let (value, dimension) = self.constant_quantity(&arguments[0].value)?;
                let rounding = self.rounding_unit(&arguments[0].value, &dimension);
                let scaled = rounding.from_canonical(&value);
                let quotient = scaled.numer() / scaled.denom();
                let remainder = scaled.numer() % scaled.denom();
                let rounded = if remainder < BigInt::zero() {
                    quotient - BigInt::one()
                } else {
                    quotient
                };
                Some((
                    rounding.to_canonical(&BigRational::from_integer(rounded)),
                    dimension,
                ))
            }
            ExprKind::Call { callee, arguments }
                if matches!(callee.path().as_deref(), Some("sum" | "min" | "max"))
                    && !arguments.is_empty()
                    && arguments.iter().all(|argument| argument.name.is_none()) =>
            {
                let aggregate = callee.path()?;
                let mut values = arguments
                    .iter()
                    .map(|argument| self.constant_quantity(&argument.value));
                let (mut value, dimension) = values.next()??;
                for item in values {
                    let (item, item_dimension) = item?;
                    if item_dimension != dimension {
                        return None;
                    }
                    match aggregate.as_str() {
                        "sum" => value += item,
                        "min" if item < value => value = item,
                        "max" if item > value => value = item,
                        "min" | "max" => {}
                        _ => return None,
                    }
                }
                Some((value, dimension))
            }
            ExprKind::Call { callee, arguments }
                if callee.path().as_deref() == Some("if_then_else")
                    && arguments.len() == 3
                    && arguments.iter().all(|argument| argument.name.is_none()) =>
            {
                if self.evaluate_predicate(&arguments[0].value)? {
                    self.constant_quantity(&arguments[1].value)
                } else {
                    self.constant_quantity(&arguments[2].value)
                }
            }
            _ => None,
        }
    }

    fn inline_pure_function_output(
        &self,
        function: &str,
        arguments: &[CallArgument],
        output: &str,
    ) -> Option<Expr> {
        let item = self.functions.get(function)?;
        let Item::Function(declaration) = &item.item else {
            return None;
        };
        let template = self
            .function_output_expressions
            .get(function)?
            .get(output)?;
        let mut substitutions = IndexMap::new();
        for argument in arguments {
            let name = if let Some(name) = &argument.name {
                name.clone()
            } else {
                declaration
                    .parameters
                    .iter()
                    .find(|parameter| !substitutions.contains_key(&parameter.name))?
                    .name
                    .clone()
            };
            substitutions.insert(name, argument.value.clone());
        }
        Some(normalize_expr(template, &substitutions))
    }

    fn artifact_name_map(&self) -> IndexMap<String, String> {
        self.artifacts
            .keys()
            .map(|name| (name.clone(), name.clone()))
            .collect()
    }

    fn span(&self, file_key: &str, span: Span) -> SourceSpan {
        let file = &self.sources.files[file_key];
        let (line, column) = line_column(&file.text, span.start);
        SourceSpan {
            path: file.display_path.clone(),
            start: span.start,
            end: span.end,
            line,
            column,
        }
    }

    fn push_error(&mut self, file_key: &str, span: Span, code: &str, message: impl Into<String>) {
        let message = message.into();
        if self.report.diagnostics.iter().any(|diagnostic| {
            diagnostic.severity == Severity::Error
                && diagnostic.code == code
                && diagnostic.span.path == self.sources.files[file_key].display_path
                && diagnostic.span.start == span.start
                && diagnostic.message == message
        }) {
            return;
        }
        self.report.diagnostics.push(Diagnostic::error(
            format!(
                "diagnostic:{}",
                stable_fingerprint(&format!("{file_key}:{code}:{}:{message}", span.start))
            ),
            code,
            message,
            self.span(file_key, span),
        ));
    }

    fn push_warning(&mut self, file_key: &str, span: Span, code: &str, message: impl Into<String>) {
        let message = message.into();
        if self.report.diagnostics.iter().any(|diagnostic| {
            diagnostic.severity == Severity::Warning
                && diagnostic.code == code
                && diagnostic.span.path == self.sources.files[file_key].display_path
                && diagnostic.span.start == span.start
                && diagnostic.message == message
        }) {
            return;
        }
        self.report.diagnostics.push(Diagnostic::warning(
            format!(
                "diagnostic:{}",
                stable_fingerprint(&format!("{file_key}:{code}:{}:{message}", span.start))
            ),
            code,
            message,
            self.span(file_key, span),
        ));
    }

    #[allow(clippy::too_many_arguments)]
    fn push_obligation(
        &mut self,
        file_key: &str,
        span: Span,
        kind: &str,
        path: String,
        expected: String,
        message: String,
        condition: Option<String>,
    ) {
        if self.report.obligations.iter().any(|obligation| {
            obligation.kind == kind && obligation.path == path && obligation.condition == condition
        }) {
            return;
        }
        self.report.obligations.push(Obligation {
            id: format!(
                "obligation:{}",
                stable_fingerprint(&format!(
                    "{kind}:{path}:{}",
                    condition.as_deref().unwrap_or("")
                ))
            ),
            kind: kind.into(),
            path,
            expected,
            message,
            span: self.span(file_key, span),
            dependencies: Vec::new(),
            condition,
        });
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum PlaceState {
    Available,
    Borrowed,
    Moved,
}

#[derive(Clone, Debug, Default)]
struct TypeEnv {
    values: IndexMap<String, Type>,
    capabilities: IndexMap<String, BTreeSet<String>>,
    lineage: IndexMap<String, BTreeSet<String>>,
    production_exprs: IndexMap<String, Expr>,
    declarations: IndexMap<String, usize>,
    expressions: IndexMap<String, Expr>,
    mutable: BTreeSet<String>,
    ownership: IndexMap<String, PlaceState>,
    configured: IndexMap<String, ConfiguredTemplate>,
    runs: IndexMap<String, RunTemplate>,
    run_dependencies: IndexMap<String, BTreeSet<String>>,
}

fn instance_endpoint_path(object_path: &str, expression: &Expr) -> Option<String> {
    match &expression.kind {
        ExprKind::Name { value } => Some(format!("{object_path}.{value}")),
        ExprKind::Member { value, member } => {
            Some(format!("{object_path}.{}.{member}", value.as_name()?))
        }
        _ => None,
    }
}

fn named_arguments(arguments: &[CallArgument]) -> IndexMap<String, Expr> {
    arguments
        .iter()
        .enumerate()
        .map(|(index, argument)| {
            (
                argument.name.clone().unwrap_or_else(|| format!("${index}")),
                argument.value.clone(),
            )
        })
        .collect()
}

/// Process calls are named by contract. A bare identifier is shorthand for
/// supplying the same-named configuration field or invocation parameter.
/// Other call sites continue to use ordinary positional argument semantics.
fn process_arguments(arguments: &[CallArgument]) -> IndexMap<String, Expr> {
    arguments
        .iter()
        .enumerate()
        .map(|(index, argument)| {
            let name = argument
                .name
                .clone()
                .or_else(|| argument.value.as_name().map(str::to_owned))
                .unwrap_or_else(|| format!("${index}"));
            (name, argument.value.clone())
        })
        .collect()
}

fn resolve_config_default(
    name: &str,
    members: &IndexMap<String, ResolvedMember>,
    supplied: &IndexMap<String, Expr>,
    environment: &TypeEnv,
    resolved: &mut IndexMap<String, Expr>,
    resolving: &mut BTreeSet<String>,
) -> Option<Expr> {
    if let Some(value) = resolved.get(name) {
        return Some(value.clone());
    }
    if let Some(value) = supplied.get(name) {
        let value = normalize_expr(value, &environment.expressions);
        resolved.insert(name.to_owned(), value.clone());
        return Some(value);
    }
    let member = members.get(name)?;
    let default = member.default.as_ref()?;
    if !resolving.insert(name.to_owned()) {
        return None;
    }
    let mut substitutions = IndexMap::new();
    for dependency in expression_names(default) {
        if members
            .get(&dependency)
            .is_some_and(|member| member.role == MemberRole::Config)
            && let Some(value) = resolve_config_default(
                &dependency,
                members,
                supplied,
                environment,
                resolved,
                resolving,
            )
        {
            substitutions.insert(dependency, value);
        }
    }
    resolving.remove(name);
    let value = normalize_expr(default, &substitutions);
    resolved.insert(name.to_owned(), value.clone());
    Some(value)
}

fn configured_process_model(configured: &ConfiguredTemplate) -> ConfiguredProcessModel {
    ConfiguredProcessModel {
        process: configured.process.clone(),
        process_source: configured.process_source.clone(),
        config: configured
            .config
            .iter()
            .map(|(name, value)| (name.clone(), value.display()))
            .collect(),
        config_sources: configured.config_sources.clone(),
        configured_arguments: configured
            .configured_arguments
            .iter()
            .map(|(name, value)| (name.clone(), configured_process_model(value)))
            .collect(),
    }
}

fn resolve_run_template(
    run: &RunTemplate,
    configured_bindings: &IndexMap<String, ConfiguredTemplate>,
) -> RunTemplate {
    let selected = run
        .process_source
        .as_ref()
        .and_then(|source| configured_bindings.get(source));
    let process = selected.map_or_else(|| run.process.clone(), |value| value.process.clone());
    let config = selected.map_or_else(|| run.config.clone(), |value| value.config.clone());
    let config_sources = selected.map_or_else(
        || run.config_sources.clone(),
        |value| value.config_sources.clone(),
    );
    let configured_arguments = selected.map_or_else(
        || run.configured_arguments.clone(),
        |value| value.configured_arguments.clone(),
    );
    RunTemplate {
        process,
        process_source: None,
        configured_by: run.configured_by.clone(),
        config,
        config_sources,
        configured_arguments: configured_arguments
            .iter()
            .map(|(name, value)| {
                (
                    name.clone(),
                    resolve_configured_template(value, configured_bindings),
                )
            })
            .collect(),
        arguments: run.arguments.clone(),
        span: run.span.clone(),
    }
}

fn resolve_configured_template(
    configured: &ConfiguredTemplate,
    configured_bindings: &IndexMap<String, ConfiguredTemplate>,
) -> ConfiguredTemplate {
    let selected = configured
        .process_source
        .as_ref()
        .and_then(|source| configured_bindings.get(source));
    let value = selected.unwrap_or(configured);
    ConfiguredTemplate {
        process: value.process.clone(),
        process_source: selected
            .is_none()
            .then(|| value.process_source.clone())
            .flatten(),
        config: value.config.clone(),
        config_sources: value.config_sources.clone(),
        configured_arguments: value
            .configured_arguments
            .iter()
            .map(|(name, nested)| {
                (
                    name.clone(),
                    resolve_configured_template(nested, configured_bindings),
                )
            })
            .collect(),
    }
}

fn collect_configured_template_processes(
    values: &IndexMap<String, ConfiguredTemplate>,
    processes: &mut BTreeSet<String>,
) {
    for value in values.values() {
        processes.insert(value.process.clone());
        collect_configured_template_processes(&value.configured_arguments, processes);
    }
}

fn type_from_dimension(dimension: Dimension, display: String, scale: Option<BigRational>) -> Type {
    if dimension.is_scalar() {
        Type::Scalar
    } else {
        Type::Quantity {
            dimension,
            display,
            scale,
            explicit_presentation: false,
            absolute: false,
        }
    }
}

fn match_generic_type(
    expected: &Type,
    actual: &Type,
    substitutions: &mut IndexMap<String, Type>,
) -> bool {
    match expected {
        Type::TypeParameter(name) => match substitutions.get(name) {
            Some(bound) => compatible(bound, actual),
            None => {
                substitutions.insert(name.clone(), actual.clone());
                true
            }
        },
        Type::Array {
            element: expected_element,
            size: expected_size,
        } => match actual {
            Type::Array {
                element: actual_element,
                size: actual_size,
            } => {
                (expected_size.is_none() || actual_size.is_none() || expected_size == actual_size)
                    && match_generic_type(expected_element, actual_element, substitutions)
            }
            _ => false,
        },
        Type::Stream(expected_element) => match actual {
            Type::Stream(actual_element) => {
                match_generic_type(expected_element, actual_element, substitutions)
            }
            _ => false,
        },
        Type::NamedTuple(expected_fields) => match actual {
            Type::NamedTuple(actual_fields) if expected_fields.len() == actual_fields.len() => {
                expected_fields.iter().all(|(name, expected)| {
                    actual_fields
                        .iter()
                        .find(|(actual_name, _)| actual_name == name)
                        .is_some_and(|(_, actual)| {
                            match_generic_type(expected, actual, substitutions)
                        })
                })
            }
            _ => false,
        },
        _ => compatible(expected, actual),
    }
}

fn substitute_generic_type(ty: &Type, substitutions: &IndexMap<String, Type>) -> Type {
    match ty {
        Type::TypeParameter(name) => substitutions
            .get(name)
            .cloned()
            .unwrap_or_else(|| ty.clone()),
        Type::Array { element, size } => Type::Array {
            element: Box::new(substitute_generic_type(element, substitutions)),
            size: size.clone(),
        },
        Type::Stream(element) => {
            Type::Stream(Box::new(substitute_generic_type(element, substitutions)))
        }
        Type::NamedTuple(fields) => Type::NamedTuple(
            fields
                .iter()
                .map(|(name, ty)| (name.clone(), substitute_generic_type(ty, substitutions)))
                .collect(),
        ),
        _ => ty.clone(),
    }
}

fn type_contains_tuple(ty: &Type) -> bool {
    match ty {
        Type::NamedTuple(_) => true,
        Type::Array { element, .. } | Type::Stream(element) => type_contains_tuple(element),
        _ => false,
    }
}

fn type_is_copyable_in_env(ty: &Type, environment: &TypeEnv) -> bool {
    match ty {
        Type::TypeParameter(_) => type_has_capability(ty, "Copyable", environment),
        Type::Array { element, .. } => type_is_copyable_in_env(element, environment),
        Type::NamedTuple(fields) => fields
            .iter()
            .all(|(_, field)| type_is_copyable_in_env(field, environment)),
        _ => ty.is_copyable(),
    }
}

fn type_is_provably_copyable(ty: &Type, bounds: &[TypeBound]) -> bool {
    match ty {
        Type::TypeParameter(name) => bounds
            .iter()
            .any(|bound| bound.parameter == *name && bound.capability.name == "Copyable"),
        Type::Array { element, .. } => type_is_provably_copyable(element, bounds),
        Type::Stream(_) => false,
        Type::NamedTuple(fields) => fields
            .iter()
            .all(|(_, ty)| type_is_provably_copyable(ty, bounds)),
        Type::Artifact(_) | Type::ConfiguredProcess(_) | Type::Run(_) | Type::Resource { .. } => {
            false
        }
        _ => true,
    }
}

fn satisfies_capability(ty: &Type, capability: &str) -> bool {
    match capability {
        "Copyable" => ty.is_copyable(),
        "Equatable" => !matches!(
            ty,
            Type::Function | Type::ConfiguredProcess(_) | Type::Run(_)
        ),
        "Ordered" | "Add" | "Subtract" | "Multiply" | "Divide" | "Power" => {
            ty.dimension().is_some()
        }
        "Iterable" => matches!(ty, Type::Array { .. } | Type::Stream(_)),
        "Indexable" => matches!(ty, Type::Array { .. }),
        _ => true,
    }
}

fn type_has_capability(ty: &Type, capability: &str, environment: &TypeEnv) -> bool {
    match ty {
        Type::TypeParameter(name) => environment
            .capabilities
            .get(name)
            .is_some_and(|capabilities| capabilities.contains(capability)),
        _ => satisfies_capability(ty, capability),
    }
}

fn is_borrowable(ty: &Type) -> bool {
    match ty {
        Type::Artifact(_) | Type::Resource { .. } => true,
        Type::Array { element, .. } => is_borrowable(element),
        _ => false,
    }
}

fn coherence_type_key(ty: &Type) -> String {
    match ty {
        Type::Quantity { dimension, .. } => format!("quantity<{dimension}>"),
        Type::Array { element, size } => {
            format!(
                "array<{},{}>",
                coherence_type_key(element),
                size.as_deref().unwrap_or("*")
            )
        }
        Type::Stream(element) => format!("stream<{}>", coherence_type_key(element)),
        Type::NamedTuple(fields) => {
            let mut fields = fields.iter().collect::<Vec<_>>();
            fields.sort_by(|(left, _), (right, _)| left.cmp(right));
            format!(
                "tuple<{}>",
                fields
                    .into_iter()
                    .map(|(name, ty)| format!("{name}:{}", coherence_type_key(ty)))
                    .collect::<Vec<_>>()
                    .join(",")
            )
        }
        _ => ty.to_string(),
    }
}

fn normalize_expr(expression: &Expr, values: &IndexMap<String, Expr>) -> Expr {
    substitute_names(expression, values)
}

fn instantiate_expression(
    expression: &Expr,
    substitutions: &IndexMap<String, Expr>,
    path_replacements: &HashMap<String, Expr>,
) -> Expr {
    let mut expression = substitute_names(expression, substitutions);
    let mut seen = BTreeSet::new();
    for _ in 0..=path_replacements.len() {
        let display = expression.display();
        if !seen.insert(display.clone()) {
            break;
        }
        let next = substitute_paths(&expression, path_replacements);
        if next.display() == display {
            return next;
        }
        expression = next;
    }
    expression
}

fn collect_flattened_aggregate_values<'a>(
    expression: &'a Expr,
    aggregate: &str,
    values: &mut Vec<&'a Expr>,
) {
    if let ExprKind::Call { callee, arguments } = &expression.kind
        && callee.path().as_deref() == Some(aggregate)
        && arguments.iter().all(|argument| argument.name.is_none())
    {
        for argument in arguments {
            collect_flattened_aggregate_values(&argument.value, aggregate, values);
        }
    } else {
        values.push(expression);
    }
}

fn call_expression(name: &str, values: Vec<Expr>, span: Span) -> Expr {
    let mut values = values
        .into_iter()
        .flat_map(|value| {
            if let ExprKind::Call { callee, arguments } = &value.kind
                && callee.path().as_deref() == Some(name)
                && arguments.iter().all(|argument| argument.name.is_none())
            {
                return arguments
                    .iter()
                    .map(|argument| argument.value.clone())
                    .collect::<Vec<_>>();
            }
            vec![value]
        })
        .collect::<Vec<_>>();
    if name == "sum" && values.len() > 1 {
        values.retain(|value| !is_zero_literal(value));
    }
    if values.len() == 1 {
        return values.pop().expect("one aggregate value");
    }
    Expr {
        kind: ExprKind::Call {
            callee: Box::new(Expr::name(name, span.clone())),
            arguments: values
                .into_iter()
                .map(|value| CallArgument {
                    name: None,
                    span: value.span.clone(),
                    value,
                })
                .collect(),
        },
        span,
    }
}

fn is_zero_literal(expression: &Expr) -> bool {
    matches!(
        &expression.kind,
        ExprKind::Number { value, .. }
            if parse_rational(value).is_some_and(|value| value.is_zero())
    )
}

fn is_one_literal(expression: &Expr) -> bool {
    matches!(
        &expression.kind,
        ExprKind::Number {
            value,
            suffix: None
        } if parse_rational(value).is_some_and(|value| value.is_one())
    )
}

fn child_run_name<'a>(expression: &'a Expr, summary: &ProcessSummary) -> Option<&'a str> {
    let ExprKind::Member { value, .. } = &expression.kind else {
        return None;
    };
    value
        .as_name()
        .filter(|name| summary.runs.contains_key(*name))
}

fn run_inputs(summary: &ProcessSummary) -> HashMap<(String, String), Expr> {
    let mut inputs = HashMap::new();
    for (source, target, _) in &summary.wires {
        if let ExprKind::Member { value, member } = &target.kind
            && let Some(run) = value.as_name()
        {
            inputs.insert((run.to_owned(), member.clone()), source.clone());
        }
    }
    inputs
}

fn substitute_names(expression: &Expr, values: &IndexMap<String, Expr>) -> Expr {
    match &expression.kind {
        ExprKind::Presentation {
            value,
            unit,
            approximate,
        } => Expr {
            kind: ExprKind::Presentation {
                value: Box::new(substitute_names(value, values)),
                unit: unit.clone(),
                approximate: *approximate,
            },
            span: expression.span.clone(),
        },
        ExprKind::NamedTuple { fields } => Expr {
            kind: ExprKind::NamedTuple {
                fields: fields
                    .iter()
                    .map(|field| CallArgument {
                        value: substitute_names(&field.value, values),
                        ..field.clone()
                    })
                    .collect(),
            },
            span: expression.span.clone(),
        },
        ExprKind::Name { value } => values
            .get(value)
            .cloned()
            .unwrap_or_else(|| expression.clone()),
        ExprKind::Unary { op, value } => Expr {
            kind: ExprKind::Unary {
                op: *op,
                value: Box::new(substitute_names(value, values)),
            },
            span: expression.span.clone(),
        },
        ExprKind::Binary { op, left, right } => Expr {
            kind: ExprKind::Binary {
                op: *op,
                left: Box::new(substitute_names(left, values)),
                right: Box::new(substitute_names(right, values)),
            },
            span: expression.span.clone(),
        },
        ExprKind::Member { value, member } => {
            let value = substitute_names(value, values);
            if let ExprKind::NamedTuple { fields } = &value.kind
                && let Some(field) = fields
                    .iter()
                    .find(|field| field.name.as_deref() == Some(member))
            {
                return field.value.clone();
            }
            Expr {
                kind: ExprKind::Member {
                    value: Box::new(value),
                    member: member.clone(),
                },
                span: expression.span.clone(),
            }
        }
        ExprKind::Index {
            value,
            index,
            wildcard,
        } => Expr {
            kind: ExprKind::Index {
                value: Box::new(substitute_names(value, values)),
                index: index
                    .as_ref()
                    .map(|index| Box::new(substitute_names(index, values))),
                wildcard: *wildcard,
            },
            span: expression.span.clone(),
        },
        ExprKind::Call { callee, arguments } => Expr {
            kind: ExprKind::Call {
                callee: Box::new(substitute_names(callee, values)),
                arguments: arguments
                    .iter()
                    .map(|argument| CallArgument {
                        name: argument.name.clone(),
                        value: substitute_names(&argument.value, values),
                        span: argument.span.clone(),
                    })
                    .collect(),
            },
            span: expression.span.clone(),
        },
        ExprKind::Array { items } => Expr {
            kind: ExprKind::Array {
                items: items
                    .iter()
                    .map(|item| substitute_names(item, values))
                    .collect(),
            },
            span: expression.span.clone(),
        },
        ExprKind::Lambda { parameters, body } => {
            let filtered = values
                .iter()
                .filter(|(name, _)| !parameters.contains(name))
                .map(|(name, value)| (name.clone(), value.clone()))
                .collect();
            Expr {
                kind: ExprKind::Lambda {
                    parameters: parameters.clone(),
                    body: Box::new(substitute_names(body, &filtered)),
                },
                span: expression.span.clone(),
            }
        }
        _ => expression.clone(),
    }
}

fn substitute_paths(expression: &Expr, replacements: &HashMap<String, Expr>) -> Expr {
    if let Some(replacement) = replacements.get(&expression.display()) {
        return replacement.clone();
    }
    if let Some(path) = expression.path()
        && let Some(replacement) = replacements.get(&path)
    {
        return replacement.clone();
    }
    match &expression.kind {
        ExprKind::Presentation {
            value,
            unit,
            approximate,
        } => Expr {
            kind: ExprKind::Presentation {
                value: Box::new(substitute_paths(value, replacements)),
                unit: unit.clone(),
                approximate: *approximate,
            },
            span: expression.span.clone(),
        },
        ExprKind::NamedTuple { fields } => Expr {
            kind: ExprKind::NamedTuple {
                fields: fields
                    .iter()
                    .map(|field| CallArgument {
                        value: substitute_paths(&field.value, replacements),
                        ..field.clone()
                    })
                    .collect(),
            },
            span: expression.span.clone(),
        },
        ExprKind::Unary { op, value } => Expr {
            kind: ExprKind::Unary {
                op: *op,
                value: Box::new(substitute_paths(value, replacements)),
            },
            span: expression.span.clone(),
        },
        ExprKind::Binary { op, left, right } => Expr {
            kind: ExprKind::Binary {
                op: *op,
                left: Box::new(substitute_paths(left, replacements)),
                right: Box::new(substitute_paths(right, replacements)),
            },
            span: expression.span.clone(),
        },
        ExprKind::Member { value, member } => {
            let value = substitute_paths(value, replacements);
            if let ExprKind::Call { arguments, .. } | ExprKind::NamedTuple { fields: arguments } =
                &value.kind
                && let Some(argument) = arguments
                    .iter()
                    .find(|argument| argument.name.as_deref() == Some(member))
            {
                return argument.value.clone();
            }
            Expr {
                kind: ExprKind::Member {
                    value: Box::new(value),
                    member: member.clone(),
                },
                span: expression.span.clone(),
            }
        }
        ExprKind::Index {
            value,
            index,
            wildcard,
        } => Expr {
            kind: ExprKind::Index {
                value: Box::new(substitute_paths(value, replacements)),
                index: index
                    .as_ref()
                    .map(|index| Box::new(substitute_paths(index, replacements))),
                wildcard: *wildcard,
            },
            span: expression.span.clone(),
        },
        ExprKind::Call { callee, arguments } => Expr {
            kind: ExprKind::Call {
                callee: Box::new(substitute_paths(callee, replacements)),
                arguments: arguments
                    .iter()
                    .map(|argument| CallArgument {
                        name: argument.name.clone(),
                        value: substitute_paths(&argument.value, replacements),
                        span: argument.span.clone(),
                    })
                    .collect(),
            },
            span: expression.span.clone(),
        },
        ExprKind::Array { items } => Expr {
            kind: ExprKind::Array {
                items: items
                    .iter()
                    .map(|item| substitute_paths(item, replacements))
                    .collect(),
            },
            span: expression.span.clone(),
        },
        ExprKind::Lambda { parameters, body } => {
            let filtered = replacements
                .iter()
                .filter(|(path, _)| {
                    path.split('.')
                        .next()
                        .is_none_or(|root| !parameters.iter().any(|parameter| parameter == root))
                })
                .map(|(path, value)| (path.clone(), value.clone()))
                .collect();
            Expr {
                kind: ExprKind::Lambda {
                    parameters: parameters.clone(),
                    body: Box::new(substitute_paths(body, &filtered)),
                },
                span: expression.span.clone(),
            }
        }
        _ => expression.clone(),
    }
}

fn expression_names(expression: &Expr) -> Vec<String> {
    let mut names = BTreeSet::new();
    collect_free_names(expression, &BTreeSet::new(), &mut names);
    names.into_iter().collect()
}

/// Return the unresolved value paths which steer an expression.
///
/// Unlike `expression_names`, this deliberately omits a call's callee: a pure
/// helper explains how a metric is calculated, but its arguments are the
/// values which prevent the metric from being calculated. Member and index
/// expressions are retained as paths so reports can say `item.package.length`
/// instead of the much less useful `item`.
fn expression_value_dependencies(expression: &Expr) -> Vec<String> {
    let mut dependencies = BTreeSet::new();
    collect_value_dependencies(expression, &BTreeSet::new(), &mut dependencies);
    dependencies.into_iter().collect()
}

fn collect_value_dependencies(
    expression: &Expr,
    bound: &BTreeSet<String>,
    dependencies: &mut BTreeSet<String>,
) {
    let is_reportable_root = |path: &str| {
        let root = path.split(['.', '[']).next().unwrap_or(path);
        !bound.contains(root)
            && !matches!(
                root,
                "true" | "false" | "math" | "every" | "if_then_else" | "is_integer"
            )
    };

    match &expression.kind {
        ExprKind::Name { value } if is_reportable_root(value) => {
            dependencies.insert(value.clone());
        }
        ExprKind::Member { value, .. } => {
            if let Some(path) = expression.path().filter(|path| is_reportable_root(path)) {
                dependencies.insert(path);
            } else {
                collect_value_dependencies(value, bound, dependencies);
            }
        }
        ExprKind::Index { value, index, .. } => {
            if let Some(path) = expression.path().filter(|path| is_reportable_root(path)) {
                dependencies.insert(path);
            } else {
                collect_value_dependencies(value, bound, dependencies);
            }
            if let Some(index) = index {
                collect_value_dependencies(index, bound, dependencies);
            }
        }
        ExprKind::Presentation { value, .. } | ExprKind::Unary { value, .. } => {
            collect_value_dependencies(value, bound, dependencies);
        }
        ExprKind::Binary { left, right, .. } => {
            collect_value_dependencies(left, bound, dependencies);
            collect_value_dependencies(right, bound, dependencies);
        }
        ExprKind::Array { items } => {
            for item in items {
                collect_value_dependencies(item, bound, dependencies);
            }
        }
        ExprKind::Call { arguments, .. } | ExprKind::NamedTuple { fields: arguments } => {
            for argument in arguments {
                collect_value_dependencies(&argument.value, bound, dependencies);
            }
        }
        ExprKind::Lambda { parameters, body } => {
            let mut nested = bound.clone();
            nested.extend(parameters.iter().cloned());
            collect_value_dependencies(body, &nested, dependencies);
        }
        _ => {}
    }
}

fn expression_run_dependencies(expression: &Expr, environment: &TypeEnv) -> BTreeSet<String> {
    expression_names(expression)
        .into_iter()
        .flat_map(|name| {
            let mut runs = environment
                .run_dependencies
                .get(&name)
                .cloned()
                .unwrap_or_default();
            if environment.runs.contains_key(&name) {
                runs.insert(name);
            }
            runs
        })
        .collect()
}

fn collect_free_names(expression: &Expr, bound: &BTreeSet<String>, names: &mut BTreeSet<String>) {
    match &expression.kind {
        ExprKind::NamedTuple { fields } => {
            for field in fields {
                collect_free_names(&field.value, bound, names);
            }
        }
        ExprKind::Name { value }
            if !bound.contains(value)
                && !matches!(
                    value.as_str(),
                    "true" | "false" | "math" | "every" | "if_then_else" | "is_integer"
                ) =>
        {
            names.insert(value.clone());
        }
        ExprKind::Presentation { value, .. }
        | ExprKind::Unary { value, .. }
        | ExprKind::Member { value, .. } => {
            collect_free_names(value, bound, names);
        }
        ExprKind::Binary { left, right, .. } => {
            collect_free_names(left, bound, names);
            collect_free_names(right, bound, names);
        }
        ExprKind::Index { value, index, .. } => {
            collect_free_names(value, bound, names);
            if let Some(index) = index {
                collect_free_names(index, bound, names);
            }
        }
        ExprKind::Array { items } => {
            for item in items {
                collect_free_names(item, bound, names);
            }
        }
        ExprKind::Call { callee, arguments } => {
            collect_free_names(callee, bound, names);
            for argument in arguments {
                collect_free_names(&argument.value, bound, names);
            }
        }
        ExprKind::Lambda { parameters, body } => {
            let mut nested = bound.clone();
            nested.extend(parameters.iter().cloned());
            collect_free_names(body, &nested, names);
        }
        _ => {}
    }
}

fn loop_cardinality(iterable: &Expr, ty: &Type) -> LoopCardinality {
    if let ExprKind::Array { items } = &iterable.kind {
        return match items.len() {
            0 => LoopCardinality::Zero,
            1 => LoopCardinality::One,
            _ => LoopCardinality::ManyOrUnknown,
        };
    }
    if let ExprKind::Call { callee, arguments } = &iterable.kind
        && callee.path().as_deref() == Some("math.range")
        && arguments.len() == 2
        && let (Some(start), Some(end)) = (
            numeric_constant(&arguments[0].value),
            numeric_constant(&arguments[1].value),
        )
    {
        if end <= start {
            return LoopCardinality::Zero;
        }
        if end - start == BigRational::one() {
            return LoopCardinality::One;
        }
        return LoopCardinality::ManyOrUnknown;
    }
    if let Type::Array {
        size: Some(size), ..
    } = ty
    {
        return match size.parse::<usize>() {
            Ok(0) => LoopCardinality::Zero,
            Ok(1) => LoopCardinality::One,
            _ => LoopCardinality::ManyOrUnknown,
        };
    }
    LoopCardinality::ManyOrUnknown
}

fn stream_cardinality_fact(expression: &Expr) -> Option<(String, Expr)> {
    let ExprKind::Binary { op, left, right } = &expression.kind else {
        return None;
    };
    if *op != BinaryOp::Eq {
        return None;
    }
    cardinality_call_path(left)
        .map(|path| (path, (**right).clone()))
        .or_else(|| cardinality_call_path(right).map(|path| (path, (**left).clone())))
}

fn cardinality_call_path(expression: &Expr) -> Option<String> {
    let ExprKind::Call { callee, arguments } = &expression.kind else {
        return None;
    };
    if callee.path().as_deref() != Some("cardinality") || arguments.len() != 1 {
        return None;
    }
    arguments[0].value.path()
}

fn integer_expression(value: i64, span: Span) -> Expr {
    Expr {
        kind: ExprKind::Number {
            value: value.to_string(),
            suffix: None,
        },
        span,
    }
}

fn binary_expression(op: BinaryOp, left: Expr, right: Expr, span: Span) -> Expr {
    Expr {
        kind: ExprKind::Binary {
            op,
            left: Box::new(left),
            right: Box::new(right),
        },
        span,
    }
}

fn cardinality_expression(iterable: &Expr, span: &Span) -> Expr {
    Expr {
        kind: ExprKind::Call {
            callee: Box::new(Expr::name("cardinality", span.clone())),
            arguments: vec![CallArgument {
                name: None,
                value: iterable.clone(),
                span: iterable.span.clone(),
            }],
        },
        span: span.clone(),
    }
}

fn effect_run_multiplicities(
    summary: &ProcessSummary,
    fallback_span: &Span,
) -> IndexMap<String, Expr> {
    fn collect(effects: &[EffectTemplate], factor: &Expr, result: &mut IndexMap<String, Expr>) {
        for effect in effects {
            match effect {
                EffectTemplate::Run { name, span } => {
                    let contribution = Expr {
                        kind: factor.kind.clone(),
                        span: span.clone(),
                    };
                    if let Some(previous) = result.get(name).cloned() {
                        result.insert(
                            name.clone(),
                            binary_expression(BinaryOp::Add, previous, contribution, span.clone()),
                        );
                    } else {
                        result.insert(name.clone(), contribution);
                    }
                }
                EffectTemplate::For {
                    iterable,
                    body,
                    span,
                    ..
                } => {
                    let count = iteration_count_expression(iterable, span)
                        .unwrap_or_else(|| cardinality_expression(iterable, span));
                    collect(
                        body,
                        &binary_expression(BinaryOp::Mul, factor.clone(), count, span.clone()),
                        result,
                    );
                }
                EffectTemplate::Repeat { count, body, span } => collect(
                    body,
                    &binary_expression(BinaryOp::Mul, factor.clone(), count.clone(), span.clone()),
                    result,
                ),
                EffectTemplate::Branch {
                    condition,
                    then_body,
                    else_body,
                    span,
                } => {
                    let conditional_factor = |condition: Expr, factor: Expr| Expr {
                        kind: ExprKind::Call {
                            callee: Box::new(Expr::name("if_then_else", span.clone())),
                            arguments: vec![
                                CallArgument {
                                    name: None,
                                    value: condition,
                                    span: span.clone(),
                                },
                                CallArgument {
                                    name: None,
                                    value: factor,
                                    span: span.clone(),
                                },
                                CallArgument {
                                    name: None,
                                    value: integer_expression(0, span.clone()),
                                    span: span.clone(),
                                },
                            ],
                        },
                        span: span.clone(),
                    };
                    collect(
                        then_body,
                        &conditional_factor(condition.clone(), factor.clone()),
                        result,
                    );
                    collect(
                        else_body,
                        &conditional_factor(
                            Expr {
                                kind: ExprKind::Unary {
                                    op: UnaryOp::Not,
                                    value: Box::new(condition.clone()),
                                },
                                span: span.clone(),
                            },
                            factor.clone(),
                        ),
                        result,
                    );
                }
                EffectTemplate::Wire { .. }
                | EffectTemplate::Assign { .. }
                | EffectTemplate::Bind { .. }
                | EffectTemplate::Yield { .. }
                | EffectTemplate::Before { .. } => {}
            }
        }
    }

    let one = integer_expression(1, fallback_span.clone());
    let mut result = IndexMap::new();
    collect(&summary.effects, &one, &mut result);
    result.retain(|_, count| !is_one_literal(count));
    result
}

/// Return the symbolic number of independent jobs represented by a finite
/// `math.range(start, end)` loop. Keeping this expression symbolic lets root
/// configuration overrides flow into the scheduler without graph unrolling.
fn iteration_count_expression(iterable: &Expr, span: &Span) -> Option<Expr> {
    let ExprKind::Call { callee, arguments } = &iterable.kind else {
        return None;
    };
    if callee.path().as_deref() != Some("math.range") || arguments.len() != 2 {
        return None;
    }
    Some(Expr {
        kind: ExprKind::Binary {
            op: BinaryOp::Sub,
            left: Box::new(arguments[1].value.clone()),
            right: Box::new(arguments[0].value.clone()),
        },
        span: span.clone(),
    })
}

fn schedule_stage_order(stages: &[String], ordering: &[(String, String)]) -> Option<Vec<String>> {
    let mut indegree = stages
        .iter()
        .map(|stage| (stage.clone(), 0_usize))
        .collect::<IndexMap<_, _>>();
    let mut successors = stages
        .iter()
        .map(|stage| (stage.clone(), Vec::<String>::new()))
        .collect::<IndexMap<_, _>>();
    for (first, second) in ordering {
        if !indegree.contains_key(first) || !indegree.contains_key(second) {
            continue;
        }
        successors.get_mut(first)?.push(second.clone());
        *indegree.get_mut(second)? += 1;
    }
    let mut ready = stages
        .iter()
        .filter(|stage| indegree.get(*stage) == Some(&0))
        .cloned()
        .collect::<Vec<_>>();
    let mut result = Vec::with_capacity(stages.len());
    while let Some(stage) = ready.first().cloned() {
        ready.remove(0);
        result.push(stage.clone());
        for successor in &successors[&stage] {
            let degree = indegree.get_mut(successor)?;
            *degree -= 1;
            if *degree == 0 {
                let source_index = stages.iter().position(|item| item == successor)?;
                let insert_at = ready
                    .iter()
                    .position(|item| {
                        stages
                            .iter()
                            .position(|stage| stage == item)
                            .unwrap_or(usize::MAX)
                            > source_index
                    })
                    .unwrap_or(ready.len());
                ready.insert(insert_at, successor.clone());
            }
        }
    }
    (result.len() == stages.len()).then_some(result)
}

fn effect_schedule_roots(effects: &[EffectTemplate]) -> Vec<&EffectTemplate> {
    fn collect<'a>(effects: &'a [EffectTemplate], result: &mut Vec<&'a EffectTemplate>) {
        for effect in effects {
            match effect {
                EffectTemplate::For { body, .. } if effects_contain_run(body) => {
                    result.push(effect)
                }
                EffectTemplate::For { body, .. } | EffectTemplate::Repeat { body, .. } => {
                    collect(body, result)
                }
                EffectTemplate::Branch {
                    then_body,
                    else_body,
                    ..
                } => {
                    collect(then_body, result);
                    collect(else_body, result);
                }
                _ => {}
            }
        }
    }

    let mut result = Vec::new();
    collect(effects, &mut result);
    result
}

fn schedule_expression_producers(
    expression: &Expr,
    run_instances: &HashMap<String, usize>,
    value_producers: &HashMap<String, BTreeSet<usize>>,
) -> BTreeSet<usize> {
    if let Some(path) = expression.path() {
        if let Some(producers) = value_producers.get(&path) {
            return producers.clone();
        }
        if let Some(root) = path.split('.').next()
            && let Some(operation) = run_instances.get(root)
        {
            return BTreeSet::from([*operation]);
        }
    }
    match &expression.kind {
        ExprKind::NamedTuple { fields } => fields
            .iter()
            .flat_map(|field| {
                schedule_expression_producers(&field.value, run_instances, value_producers)
            })
            .collect(),
        ExprKind::Presentation { value, .. }
        | ExprKind::Unary { value, .. }
        | ExprKind::Member { value, .. } => {
            schedule_expression_producers(value, run_instances, value_producers)
        }
        ExprKind::Binary { left, right, .. } => {
            let mut producers = schedule_expression_producers(left, run_instances, value_producers);
            producers.extend(schedule_expression_producers(
                right,
                run_instances,
                value_producers,
            ));
            producers
        }
        ExprKind::Index { value, index, .. } => {
            let mut producers =
                schedule_expression_producers(value, run_instances, value_producers);
            if let Some(index) = index {
                producers.extend(schedule_expression_producers(
                    index,
                    run_instances,
                    value_producers,
                ));
            }
            producers
        }
        ExprKind::Call { callee, arguments } => {
            let mut producers =
                schedule_expression_producers(callee, run_instances, value_producers);
            for argument in arguments {
                producers.extend(schedule_expression_producers(
                    &argument.value,
                    run_instances,
                    value_producers,
                ));
            }
            producers
        }
        ExprKind::Lambda { body, .. } => {
            schedule_expression_producers(body, run_instances, value_producers)
        }
        ExprKind::Array { items } => items.iter().fold(BTreeSet::new(), |mut result, item| {
            result.extend(schedule_expression_producers(
                item,
                run_instances,
                value_producers,
            ));
            result
        }),
        _ => BTreeSet::new(),
    }
}

fn effects_contain_run(effects: &[EffectTemplate]) -> bool {
    effects.iter().any(|effect| match effect {
        EffectTemplate::Run { .. } => true,
        EffectTemplate::For { body, .. } | EffectTemplate::Repeat { body, .. } => {
            effects_contain_run(body)
        }
        EffectTemplate::Branch {
            then_body,
            else_body,
            ..
        } => effects_contain_run(then_body) || effects_contain_run(else_body),
        _ => false,
    })
}

fn dedup_schedule_dependencies(dependencies: &mut Vec<ScheduleDependency>) {
    let mut unique = Vec::with_capacity(dependencies.len());
    for dependency in dependencies.drain(..) {
        if !unique.contains(&dependency) {
            unique.push(dependency);
        }
    }
    *dependencies = unique;
}

/// Deterministic earliest-start list scheduling for identical finite jobs.
/// Each job follows the same stage sequence for every repetition. Distinct
/// stage pools may operate concurrently, allowing one item to rework while a
/// different item occupies the processing lane.
fn schedule_repeated_jobs(
    jobs: u64,
    repetitions: u64,
    stages: &[ConcreteScheduleStage],
) -> ScheduledJobsResult {
    let operation_count = repetitions as usize * stages.len();
    let mut job_next = vec![0_usize; jobs as usize];
    let mut job_finish = vec![BigRational::zero(); jobs as usize];
    let mut ready = stages
        .iter()
        .map(|_| VecDeque::<usize>::new())
        .collect::<Vec<_>>();
    ready[0].extend(0..jobs as usize);
    let mut active = stages
        .iter()
        .map(|_| Vec::<(BigRational, usize)>::new())
        .collect::<Vec<_>>();
    let mut results = stages
        .iter()
        .map(|_| ScheduledStageResult {
            operation_count: 0,
            earliest_ready: BigRational::zero(),
            earliest_start: BigRational::zero(),
            latest_finish: BigRational::zero(),
            occupied_duration: BigRational::zero(),
        })
        .collect::<Vec<_>>();
    let total = jobs as usize * operation_count;
    let mut intervals = Vec::with_capacity(total);
    let mut completed = 0_usize;
    let mut now = BigRational::zero();
    while completed < total {
        // Complete every operation at this event time, making its next stage
        // ready before capacity is reassigned.
        for stage_active in &mut active {
            let mut remaining = Vec::new();
            for (finish, job) in stage_active.drain(..) {
                if finish <= now {
                    completed += 1;
                    job_next[job] += 1;
                    job_finish[job] = finish;
                    if job_next[job] < operation_count {
                        ready[job_next[job] % stages.len()].push_back(job);
                    }
                } else {
                    remaining.push((finish, job));
                }
            }
            *stage_active = remaining;
        }

        // Downstream-first dispatch keeps a bottleneck fed while upstream
        // stages prepare other jobs concurrently.
        for stage_index in (0..stages.len()).rev() {
            let available = stages[stage_index]
                .parallel_capacity
                .saturating_sub(active[stage_index].len() as u64);
            for _ in 0..available {
                let Some(job) = ready[stage_index].pop_front() else {
                    break;
                };
                let operation_duration = stages[stage_index]
                    .job_durations
                    .as_ref()
                    .and_then(|durations| durations.get(job))
                    .unwrap_or(&stages[stage_index].duration);
                let operation_ready = job_finish[job].clone();
                let finish = &now + operation_duration;
                active[stage_index].push((finish.clone(), job));
                let result = &mut results[stage_index];
                if result.operation_count == 0 || operation_ready < result.earliest_ready {
                    result.earliest_ready = operation_ready.clone();
                }
                if result.operation_count == 0 || now < result.earliest_start {
                    result.earliest_start = now.clone();
                }
                result.operation_count += 1;
                result.occupied_duration += operation_duration;
                if finish > result.latest_finish {
                    result.latest_finish = finish.clone();
                }
                intervals.push(ScheduledOperationInterval {
                    stage: stage_index,
                    ready: operation_ready,
                    start: now.clone(),
                    active_finish: finish.clone(),
                    finish,
                });
            }
        }

        if completed < total {
            let next = active
                .iter()
                .flatten()
                .map(|(finish, _)| finish)
                .min()
                .cloned();
            let Some(next) = next else {
                break;
            };
            now = next;
        }
    }
    ScheduledJobsResult {
        makespan: job_finish
            .into_iter()
            .max()
            .unwrap_or_else(BigRational::zero),
        stages: results,
        intervals,
        complete: completed == total,
    }
}

/// Earliest-start scheduling for a finite per-job physical operation DAG.
/// Resource occupancy is checked globally by pool path, so two distinct
/// operation templates may safely share one divisible resource.
fn schedule_effect_jobs(
    jobs: u64,
    operations: &[ConcreteScheduleOperation],
    stages: &[ConcreteScheduleStage],
) -> ScheduledJobsResult {
    #[derive(Clone, Copy)]
    enum Event {
        Completion { job: usize, operation: usize },
        Handoff { job: usize, operation: usize },
    }

    fn release_condition(
        remaining: &mut [Vec<usize>],
        ready: &mut [BTreeSet<usize>],
        ready_since: &mut [Vec<Option<BigRational>>],
        now: &BigRational,
        job: usize,
        operation: usize,
    ) {
        if remaining[job][operation] == 0 {
            return;
        }
        remaining[job][operation] -= 1;
        if remaining[job][operation] == 0 {
            ready[operation].insert(job);
            ready_since[job][operation] = Some(now.clone());
        }
    }

    let job_count = jobs as usize;
    let mut starts = vec![vec![None::<BigRational>; operations.len()]; job_count];
    let mut finishes = vec![vec![None::<BigRational>; operations.len()]; job_count];
    let mut remaining = vec![
        operations
            .iter()
            .map(|operation| operation.dependencies.len())
            .collect::<Vec<_>>();
        job_count
    ];
    let mut completion_successors = vec![Vec::<usize>::new(); operations.len()];
    let mut close_successors = vec![Vec::<usize>::new(); operations.len()];
    let mut handoff_successors = vec![Vec::<(usize, BigRational)>::new(); operations.len()];
    for (operation, value) in operations.iter().enumerate() {
        for dependency in &value.dependencies {
            match dependency {
                ScheduleDependency::Completion(predecessor) => {
                    completion_successors[*predecessor].push(operation);
                }
                ScheduleDependency::Close(predecessor) => {
                    close_successors[*predecessor].push(operation);
                }
                ScheduleDependency::Handoff {
                    operation: predecessor,
                    delay,
                } => {
                    handoff_successors[*predecessor].push((operation, delay.clone()));
                }
            }
        }
    }
    // All jobs waiting on the same operation have identical resource claims.
    // Keep one ordered queue per operation so each scheduling event examines
    // operation templates, not every ready job. The previous flat set caused
    // quadratic scans for large streamed batches (thousands of ready jobs at
    // thousands of event times).
    let mut ready = vec![BTreeSet::<usize>::new(); operations.len()];
    let mut ready_since = vec![vec![None::<BigRational>; operations.len()]; job_count];
    for (job, job_remaining) in remaining.iter().enumerate() {
        for (operation, dependencies) in job_remaining.iter().enumerate() {
            if *dependencies == 0 {
                ready[operation].insert(job);
                ready_since[job][operation] = Some(BigRational::zero());
            }
        }
    }
    let mut events = BTreeMap::<BigRational, Vec<Event>>::new();
    let mut active_stages = vec![0_u64; stages.len()];
    let mut resource_in_use = HashMap::<String, BigRational>::new();
    let mut results = stages
        .iter()
        .map(|_| ScheduledStageResult {
            operation_count: 0,
            earliest_ready: BigRational::zero(),
            earliest_start: BigRational::zero(),
            latest_finish: BigRational::zero(),
            occupied_duration: BigRational::zero(),
        })
        .collect::<Vec<_>>();
    let total = job_count * operations.len();
    let mut intervals = Vec::with_capacity(total);
    let mut completed = 0_usize;
    let mut now = BigRational::zero();
    let mut makespan = BigRational::zero();
    while completed < total {
        if let Some(current_events) = events.remove(&now) {
            for event in current_events {
                match event {
                    Event::Completion { job, operation } => {
                        completed += 1;
                        let stage_index = operations[operation].stage;
                        active_stages[stage_index] = active_stages[stage_index].saturating_sub(1);
                        for resource in &stages[stage_index].resources {
                            let in_use = resource_in_use
                                .entry(resource.path.clone())
                                .or_insert_with(BigRational::zero);
                            *in_use -= &resource.claim;
                        }
                        for successor in &completion_successors[operation] {
                            release_condition(
                                &mut remaining,
                                &mut ready,
                                &mut ready_since,
                                &now,
                                job,
                                *successor,
                            );
                        }
                    }
                    Event::Handoff { job, operation } => {
                        release_condition(
                            &mut remaining,
                            &mut ready,
                            &mut ready_since,
                            &now,
                            job,
                            operation,
                        );
                    }
                }
            }
        }

        let mut dispatched = false;
        // A close dependency released by a dispatch was not part of this
        // event's original ready snapshot. Defer it to the next pass at the
        // same timestamp so operation priority remains exactly deterministic.
        let mut deferred_ready = vec![BTreeSet::<usize>::new(); operations.len()];
        for operation_index in 0..operations.len() {
            let stage_index = operations[operation_index].stage;
            loop {
                if ready[operation_index].is_empty()
                    || active_stages[stage_index] >= stages[stage_index].parallel_capacity
                    || stages[stage_index].resources.iter().any(|requested| {
                        resource_in_use
                            .get(&requested.path)
                            .cloned()
                            .unwrap_or_else(BigRational::zero)
                            + &requested.claim
                            > requested.capacity
                    })
                {
                    break;
                }
                let job = ready[operation_index]
                    .pop_first()
                    .expect("ready operation has a job");
                let close_floor = operations[operation_index]
                    .dependencies
                    .iter()
                    .filter_map(|dependency| match dependency {
                        ScheduleDependency::Close(predecessor) => {
                            finishes[job][*predecessor].clone()
                        }
                        _ => None,
                    })
                    .max()
                    .unwrap_or_else(|| now.clone());
                let operation_duration = stages[stage_index]
                    .job_durations
                    .as_ref()
                    .and_then(|durations| durations.get(job))
                    .unwrap_or(&stages[stage_index].duration);
                let operation_ready = ready_since[job][operation_index]
                    .take()
                    .unwrap_or_else(|| now.clone());
                let active_finish = &now + operation_duration;
                let finish = (&now + operation_duration).max(close_floor);
                starts[job][operation_index] = Some(now.clone());
                finishes[job][operation_index] = Some(finish.clone());
                makespan = makespan.max(finish.clone());
                active_stages[stage_index] += 1;
                for resource in &stages[stage_index].resources {
                    *resource_in_use
                        .entry(resource.path.clone())
                        .or_insert_with(BigRational::zero) += &resource.claim;
                }
                let result = &mut results[stage_index];
                if result.operation_count == 0 || operation_ready < result.earliest_ready {
                    result.earliest_ready = operation_ready.clone();
                }
                if result.operation_count == 0 || now < result.earliest_start {
                    result.earliest_start = now.clone();
                }
                result.operation_count += 1;
                result.occupied_duration += &finish - &now;
                if finish > result.latest_finish {
                    result.latest_finish = finish.clone();
                }
                intervals.push(ScheduledOperationInterval {
                    stage: stage_index,
                    ready: operation_ready,
                    start: now.clone(),
                    active_finish,
                    finish: finish.clone(),
                });
                events.entry(finish).or_default().push(Event::Completion {
                    job,
                    operation: operation_index,
                });
                for successor in &close_successors[operation_index] {
                    release_condition(
                        &mut remaining,
                        &mut deferred_ready,
                        &mut ready_since,
                        &now,
                        job,
                        *successor,
                    );
                }
                for (successor, delay) in &handoff_successors[operation_index] {
                    events
                        .entry(&now + delay)
                        .or_default()
                        .push(Event::Handoff {
                            job,
                            operation: *successor,
                        });
                }
                dispatched = true;
            }
        }
        for (ready, deferred) in ready.iter_mut().zip(&mut deferred_ready) {
            ready.append(deferred);
        }

        if completed < total {
            if events.contains_key(&now) || dispatched {
                continue;
            }
            let Some(next) = events.keys().next().cloned() else {
                break;
            };
            if next < now {
                break;
            }
            now = next;
        }
    }

    ScheduledJobsResult {
        makespan,
        stages: results,
        intervals,
        complete: completed == total,
    }
}

/// Collapse concrete operation intervals into a small, deterministic profile
/// suitable for reports and the browser Gantt. Service work, stream-close
/// waiting, and resource-queue backlog remain distinct instead of turning a
/// repeated streamed stage into one misleading full-width bar.
fn schedule_activity_profiles(
    scheduled: &ScheduledJobsResult,
    absolute_offset: &BigRational,
    stage_count: usize,
) -> Vec<Vec<ResourceScheduleActivity>> {
    const BUCKET_COUNT: usize = 96;

    #[derive(Clone, Copy, Default)]
    struct Delta {
        active: i64,
        waiting: i64,
        queued: i64,
    }

    fn add_interval(
        events: &mut HashMap<BigRational, Delta>,
        start: &BigRational,
        finish: &BigRational,
        state: fn(&mut Delta) -> &mut i64,
    ) {
        if finish <= start {
            return;
        }
        *state(events.entry(start.clone()).or_default()) += 1;
        *state(events.entry(finish.clone()).or_default()) -= 1;
    }

    fn add_area(
        areas: &mut [BigRational],
        start: &BigRational,
        finish: &BigRational,
        count: i64,
        width: &BigRational,
    ) {
        if count <= 0 || finish <= start || width.is_zero() {
            return;
        }
        let count = BigRational::from_integer(BigInt::from(count));
        let mut cursor = start.clone();
        while cursor < *finish {
            let index = (&cursor / width)
                .to_integer()
                .to_usize()
                .unwrap_or(areas.len().saturating_sub(1))
                .min(areas.len().saturating_sub(1));
            let bucket_end =
                width * BigRational::from_integer(BigInt::from(index.saturating_add(1)));
            let segment_end = finish.clone().min(bucket_end);
            if segment_end <= cursor {
                break;
            }
            areas[index] += (&segment_end - &cursor) * &count;
            cursor = segment_end;
        }
    }

    if scheduled.makespan <= BigRational::zero() || stage_count == 0 {
        return vec![Vec::new(); stage_count];
    }
    let width = &scheduled.makespan / BigRational::from_integer(BigInt::from(BUCKET_COUNT));
    let mut events = vec![HashMap::<BigRational, Delta>::new(); stage_count];
    for interval in &scheduled.intervals {
        let Some(stage_events) = events.get_mut(interval.stage) else {
            continue;
        };
        add_interval(stage_events, &interval.ready, &interval.start, |delta| {
            &mut delta.queued
        });
        add_interval(
            stage_events,
            &interval.start,
            &interval.active_finish,
            |delta| &mut delta.active,
        );
        add_interval(
            stage_events,
            &interval.active_finish,
            &interval.finish,
            |delta| &mut delta.waiting,
        );
    }

    events
        .into_iter()
        .map(|stage_events| {
            let mut active_area = vec![BigRational::zero(); BUCKET_COUNT];
            let mut waiting_area = vec![BigRational::zero(); BUCKET_COUNT];
            let mut queued_area = vec![BigRational::zero(); BUCKET_COUNT];
            let mut active = 0_i64;
            let mut waiting = 0_i64;
            let mut queued = 0_i64;
            let mut previous = BigRational::zero();
            let mut stage_events = stage_events.into_iter().collect::<Vec<_>>();
            stage_events.sort_unstable_by(|left, right| left.0.cmp(&right.0));
            for (time, delta) in stage_events {
                add_area(&mut active_area, &previous, &time, active, &width);
                add_area(&mut waiting_area, &previous, &time, waiting, &width);
                add_area(&mut queued_area, &previous, &time, queued, &width);
                active += delta.active;
                waiting += delta.waiting;
                queued += delta.queued;
                previous = time;
            }

            (0..BUCKET_COUNT)
                .filter_map(|index| {
                    if active_area[index].is_zero()
                        && waiting_area[index].is_zero()
                        && queued_area[index].is_zero()
                    {
                        return None;
                    }
                    let local_start = &width * BigRational::from_integer(BigInt::from(index));
                    let local_end = &width * BigRational::from_integer(BigInt::from(index + 1));
                    Some(ResourceScheduleActivity {
                        start_seconds: format_exact_rational(&(absolute_offset + local_start)),
                        end_seconds: format_exact_rational(&(absolute_offset + local_end)),
                        average_active_operations: format_exact_rational(
                            &(&active_area[index] / &width),
                        ),
                        average_waiting_operations: format_exact_rational(
                            &(&waiting_area[index] / &width),
                        ),
                        average_queued_operations: format_exact_rational(
                            &(&queued_area[index] / &width),
                        ),
                    })
                })
                .collect()
        })
        .collect()
}

fn quantify_over(iterable: &Expr, binding: &str, predicate: &Expr, span: &Span) -> Expr {
    Expr {
        kind: ExprKind::Call {
            callee: Box::new(Expr::name("every", span.clone())),
            arguments: vec![
                CallArgument {
                    name: None,
                    value: iterable.clone(),
                    span: iterable.span.clone(),
                },
                CallArgument {
                    name: None,
                    value: Expr {
                        kind: ExprKind::Lambda {
                            parameters: vec![binding.to_owned()],
                            body: Box::new(predicate.clone()),
                        },
                        span: span.clone(),
                    },
                    span: span.clone(),
                },
            ],
        },
        span: span.clone(),
    }
}

/// Compile a straight-line pure function into symbolic named-result
/// expressions. Control-flow-heavy functions remain valid and simply stay
/// residual; the exact evaluator only needs this conservative subset.
fn count_block_requirements(block: &Block) -> usize {
    block
        .statements
        .iter()
        .map(|statement| match statement {
            Stmt::Requires { .. } => 1,
            Stmt::For { body, .. } | Stmt::Repeat { body, .. } => count_block_requirements(body),
            Stmt::If {
                then_branch,
                else_branch,
                ..
            } => {
                count_block_requirements(then_branch)
                    + else_branch.as_ref().map_or(0, count_block_requirements)
            }
            _ => 0,
        })
        .sum()
}

fn symbolic_refinement_requirements(
    block: &Block,
    values: &mut IndexMap<String, Expr>,
) -> Option<Vec<Expr>> {
    let mut predicates = vec![];
    for statement in &block.statements {
        match statement {
            Stmt::Let { name, value, .. } => {
                values.insert(name.clone(), normalize_expr(value, values));
            }
            Stmt::Assign { target, value, .. } => {
                if let Some(name) = target.as_name() {
                    values.insert(name.to_owned(), normalize_expr(value, values));
                }
            }
            Stmt::Requires { value, .. } => predicates.push(normalize_expr(value, values)),
            Stmt::If {
                condition,
                then_branch,
                else_branch,
                span,
            } => {
                let condition = normalize_expr(condition, values);
                let mut then_values = values.clone();
                let mut else_values = values.clone();
                for (negated, branch, branch_values) in [
                    (true, Some(then_branch), &mut then_values),
                    (false, else_branch.as_ref(), &mut else_values),
                ] {
                    if let Some(branch) = branch {
                        for predicate in symbolic_refinement_requirements(branch, branch_values)? {
                            let guard = if negated {
                                Expr {
                                    kind: ExprKind::Unary {
                                        op: UnaryOp::Not,
                                        value: Box::new(condition.clone()),
                                    },
                                    span: span.clone(),
                                }
                            } else {
                                condition.clone()
                            };
                            predicates.push(Expr {
                                kind: ExprKind::Binary {
                                    op: BinaryOp::Or,
                                    left: Box::new(guard),
                                    right: Box::new(predicate),
                                },
                                span: span.clone(),
                            });
                        }
                    }
                }
                for (name, value) in values.clone() {
                    let then_value = then_values.get(&name).unwrap_or(&value);
                    let else_value = else_values.get(&name).unwrap_or(&value);
                    values.insert(
                        name,
                        Expr {
                            kind: ExprKind::Call {
                                callee: Box::new(Expr::name("if_then_else", span.clone())),
                                arguments: [&condition, then_value, else_value]
                                    .into_iter()
                                    .map(|value| CallArgument {
                                        name: None,
                                        value: value.clone(),
                                        span: span.clone(),
                                    })
                                    .collect(),
                            },
                            span: span.clone(),
                        },
                    );
                }
            }
            Stmt::For {
                binding,
                iterable,
                body,
                span,
            } => {
                let iterable = normalize_expr(iterable, values);
                let concrete = match &iterable.kind {
                    ExprKind::Array { items } if items.len() <= 1024 => Some(items.clone()),
                    ExprKind::Call { callee, arguments }
                        if callee.path().as_deref() == Some("math.range")
                            && arguments.len() == 2 =>
                    {
                        numeric_constant(&arguments[0].value).and_then(|start| {
                            let end = numeric_constant(&arguments[1].value)?;
                            if !start.is_integer() || !end.is_integer() {
                                return None;
                            }
                            let start = start.to_integer().to_i64()?;
                            let end = end.to_integer().to_i64()?;
                            (end.saturating_sub(start) <= 1024).then(|| {
                                (start..end)
                                    .map(|n| integer_expression(n, span.clone()))
                                    .collect()
                            })
                        })
                    }
                    _ => None,
                };
                if let Some(items) = concrete {
                    for item in items {
                        let mut nested = values.clone();
                        nested.insert(binding.clone(), item);
                        predicates.extend(symbolic_refinement_requirements(body, &mut nested)?);
                        for (name, value) in values.iter_mut() {
                            if let Some(updated) = nested.get(name) {
                                *value = updated.clone();
                            }
                        }
                    }
                    continue;
                }
                let mut nested = values.clone();
                nested.shift_remove(binding);
                let requirements = symbolic_refinement_requirements(body, &mut nested)?;
                if values.iter().any(|(name, value)| {
                    nested
                        .get(name)
                        .is_some_and(|updated| updated.display() != value.display())
                }) {
                    return None;
                }
                for predicate in requirements {
                    predicates.push(quantify_over(&iterable, binding, &predicate, span));
                }
            }
            _ => {}
        }
    }
    Some(predicates)
}

fn symbolic_pure_function_outputs(function: &Function) -> Option<IndexMap<String, Expr>> {
    let body = function.body.as_ref()?;
    let output_names = function
        .outputs
        .iter()
        .map(|output| output.name.as_str())
        .collect::<BTreeSet<_>>();
    let mut substitutions = function
        .parameters
        .iter()
        .map(|parameter| {
            (
                parameter.name.clone(),
                Expr::name(&parameter.name, parameter.span.clone()),
            )
        })
        .collect::<IndexMap<_, _>>();
    let mut outputs = IndexMap::new();
    for statement in &body.statements {
        match statement {
            Stmt::Let { name, value, .. } => {
                substitutions.insert(name.clone(), normalize_expr(value, &substitutions));
            }
            Stmt::Assign { target, value, .. } => {
                let name = target.as_name()?;
                let value = normalize_expr(value, &substitutions);
                if output_names.contains(name) {
                    outputs.insert(name.to_owned(), value.clone());
                }
                substitutions.insert(name.to_owned(), value);
            }
            Stmt::Requires { .. } => {}
            _ => return None,
        }
    }
    (outputs.len() == function.outputs.len()).then_some(outputs)
}

fn iterable_requirement_truth(iterable: &Expr, binding: &str, predicate: &Expr) -> Option<bool> {
    if let ExprKind::Array { items } = &iterable.kind {
        if items.is_empty() {
            return Some(true);
        }
        if let Some(constant) = constant_bool(predicate) {
            return Some(constant);
        }
        let mut replacements = IndexMap::new();
        for item in items {
            replacements.insert(binding.to_owned(), item.clone());
            match constant_bool(&substitute_names(predicate, &replacements)) {
                Some(true) => {}
                other => return other,
            }
        }
        return Some(true);
    }
    let ExprKind::Call { callee, arguments } = &iterable.kind else {
        return None;
    };
    if callee.path().as_deref() != Some("math.range") || arguments.len() != 2 {
        return None;
    }
    let start = numeric_constant(&arguments[0].value)?;
    let end = numeric_constant(&arguments[1].value)?;
    if !start.is_integer() || !end.is_integer() {
        return None;
    }
    let start = start.to_integer().to_i64()?;
    let end = end.to_integer().to_i64()?;
    if end <= start {
        return Some(true);
    }
    if let Some(constant) = constant_bool(predicate) {
        return Some(constant);
    }
    if end.saturating_sub(start) > 1024 {
        return None;
    }
    let mut replacements = IndexMap::new();
    for value in start..end {
        replacements.insert(
            binding.to_owned(),
            Expr {
                kind: ExprKind::Number {
                    value: value.to_string(),
                    suffix: None,
                },
                span: predicate.span.clone(),
            },
        );
        if constant_bool(&substitute_names(predicate, &replacements)) != Some(true) {
            return constant_bool(&substitute_names(predicate, &replacements));
        }
    }
    Some(true)
}

fn constant_bool(expression: &Expr) -> Option<bool> {
    match &expression.kind {
        ExprKind::Bool { value } => Some(*value),
        ExprKind::Unary {
            op: UnaryOp::Not,
            value,
        } => constant_bool(value).map(|value| !value),
        ExprKind::Binary {
            op: BinaryOp::And,
            left,
            right,
        } => match (constant_bool(left), constant_bool(right)) {
            (Some(false), _) | (_, Some(false)) => Some(false),
            (Some(true), Some(true)) => Some(true),
            _ => None,
        },
        ExprKind::Binary {
            op: BinaryOp::Or,
            left,
            right,
        } => match (constant_bool(left), constant_bool(right)) {
            (Some(true), _) | (_, Some(true)) => Some(true),
            (Some(false), Some(false)) => Some(false),
            _ => None,
        },
        ExprKind::Binary { op, left, right } => {
            let left = numeric_constant(left)?;
            let right = numeric_constant(right)?;
            match op {
                BinaryOp::Eq => Some(left == right),
                BinaryOp::NotEq => Some(left != right),
                BinaryOp::Lt => Some(left < right),
                BinaryOp::Le => Some(left <= right),
                BinaryOp::Gt => Some(left > right),
                BinaryOp::Ge => Some(left >= right),
                BinaryOp::And
                | BinaryOp::Or
                | BinaryOp::Add
                | BinaryOp::Sub
                | BinaryOp::Mul
                | BinaryOp::Div
                | BinaryOp::Pow => None,
            }
        }
        _ => None,
    }
}

/// Rewrite `-<number>` into the equivalent negative literal.
///
/// Only literals qualify. This keeps a leading sign out of the affine
/// conversion path, where negating a canonical value is not the same thing as
/// reading a negative magnitude on an offset scale.
fn negated_number_literal(expression: &Expr) -> Option<Expr> {
    let ExprKind::Number { value, suffix } = &expression.kind else {
        return None;
    };
    Some(Expr {
        kind: ExprKind::Number {
            value: format!("-{value}"),
            suffix: suffix.clone(),
        },
        span: expression.span.clone(),
    })
}

fn numeric_constant(expression: &Expr) -> Option<BigRational> {
    match &expression.kind {
        ExprKind::Presentation { value, .. } => numeric_constant(value),
        ExprKind::Number { value, .. } => parse_rational(value),
        ExprKind::Unary {
            op: UnaryOp::Neg,
            value,
        } => numeric_constant(value).map(|value| -value),
        ExprKind::Unary {
            op: UnaryOp::Pos,
            value,
        } => numeric_constant(value),
        _ => None,
    }
}

/// Returns the exact canonical size and dimension of one unit in an
/// expression's resolved presentation scale. Dimensionless expressions always
/// collapse to scale one: `Meter / Millimeter` is a scalar count, not a scalar
/// quantized in thousandths. Addition keeps its left operand's scale, while
/// multiplication and division compose scales algebraically.
fn preferred_unit(expression: &Expr, registry: &TypeRegistry) -> Option<(BigRational, Dimension)> {
    match &expression.kind {
        ExprKind::Presentation { value, unit, .. } => {
            preferred_unit(unit.as_deref().unwrap_or(value), registry)
        }
        ExprKind::Name { value } => registry
            .units
            .get(value)
            .map(|unit| (unit.scale.clone(), unit.dimension.clone())),
        ExprKind::Number { suffix, .. } => suffix.as_ref().map_or_else(
            || Some((BigRational::one(), Dimension::scalar())),
            |suffix| {
                registry
                    .units
                    .get(suffix)
                    .map(|unit| (unit.scale.clone(), unit.dimension.clone()))
            },
        ),
        ExprKind::Unary { value, .. } => preferred_unit(value, registry),
        ExprKind::Binary {
            op: BinaryOp::Add | BinaryOp::Sub,
            left,
            right,
        } => {
            let left = preferred_unit(left, registry)?;
            let right = preferred_unit(right, registry)?;
            (left.1 == right.1).then_some(left)
        }
        ExprKind::Binary { op, left, right } if matches!(op, BinaryOp::Mul | BinaryOp::Div) => {
            let (left_scale, left_dimension) = preferred_unit(left, registry)?;
            let (right_scale, right_dimension) = preferred_unit(right, registry)?;
            let (scale, dimension) = if *op == BinaryOp::Mul {
                (
                    left_scale * right_scale,
                    left_dimension.multiply(&right_dimension),
                )
            } else {
                (
                    left_scale / right_scale,
                    left_dimension.divide(&right_dimension),
                )
            };
            Some(if dimension.is_scalar() {
                (BigRational::one(), dimension)
            } else {
                (scale, dimension)
            })
        }
        ExprKind::Binary {
            op: BinaryOp::Pow,
            left,
            right,
        } => {
            let (scale, dimension) = preferred_unit(left, registry)?;
            let exponent = numeric_constant(right)?;
            if !exponent.denom().is_one() {
                return None;
            }
            let scale = scale.pow(exponent.numer().to_i32()?);
            let dimension = dimension.pow(&exponent);
            Some(if dimension.is_scalar() {
                (BigRational::one(), dimension)
            } else {
                (scale, dimension)
            })
        }
        ExprKind::Call { callee, arguments }
            if matches!(
                callee.path().as_deref(),
                Some("ceil" | "math.ceil" | "floor" | "math.floor" | "min" | "max")
            ) =>
        {
            let (scale, dimension) = arguments
                .first()
                .and_then(|argument| preferred_unit(&argument.value, registry))?;
            Some(if dimension.is_scalar() {
                (BigRational::one(), dimension)
            } else {
                (scale, dimension)
            })
        }
        ExprKind::Call { callee, arguments }
            if callee.path().as_deref() == Some("if_then_else") =>
        {
            arguments
                .get(1)
                .and_then(|argument| preferred_unit(&argument.value, registry))
        }
        ExprKind::Array { items } => items
            .first()
            .and_then(|item| preferred_unit(item, registry)),
        _ => None,
    }
}

/// Preserve and normalize the concrete unit spelling selected by a constant
/// expression even when its whole dimension has no named alias. This both
/// avoids inventing a unit name and cancels obvious factors: `Centimeter^3 /
/// Centimeter^2 / Hour` renders as `Centimeter / Hour`.
fn preferred_quantity_presentation(
    expression: &Expr,
    registry: &TypeRegistry,
) -> Option<(BigRational, String)> {
    let (scale, dimension) = preferred_unit(expression, registry)?;
    if let Some((normalized_scale, label)) =
        normalized_dimension_presentation(expression, &dimension, registry)
    {
        // `Kilogram * Meter^2 / Second^2` is a Joule -- but only say so when the
        // author's own base units are the coherent SI ones. A cgs expression
        // normalizes to scale 1e-7, and rewriting it as `1/10000000 Joule`
        // would replace a spelling the author chose with a worse one.
        if normalized_scale.is_one()
            && let Some(canonical) = registry.canonical_derived_unit(&dimension)
        {
            return Some((BigRational::one(), canonical.name.clone()));
        }
        return Some((normalized_scale, label));
    }
    let label = format_unit_factors(&preferred_unit_factors(expression, registry)?);
    (!dimension.is_scalar()).then_some((scale, label))
}

/// Re-express a quantity's final dimension in the first simple unit selected
/// for each base dimension. This makes aliases such as `Centimeter` and
/// `Meter` comparable before formatting: `Centimeter^3 / Meter^2 / Hour`
/// becomes `Centimeter / Hour`, with the exact scale adjusted accordingly.
fn normalized_dimension_presentation(
    expression: &Expr,
    dimension: &Dimension,
    registry: &TypeRegistry,
) -> Option<(BigRational, String)> {
    let mut candidates = BTreeMap::new();
    collect_preferred_base_units(expression, registry, &mut candidates);
    let mut factors = BTreeMap::new();
    let mut scale = BigRational::one();
    for (base, exponent) in &dimension.0 {
        if !exponent.denom().is_one() {
            return None;
        }
        let exponent = exponent.numer().to_i32()?;
        let (unit, unit_scale) = candidates.get(base)?;
        *factors.entry(unit.clone()).or_default() += exponent;
        scale *= unit_scale.clone().pow(exponent);
    }
    factors.retain(|_, exponent| *exponent != 0);
    (!factors.is_empty()).then(|| (scale, format_unit_factors(&factors)))
}

/// Record the concrete unit an author prefers for each base dimension.
///
/// A *simple* unit (`Meter`, `Gigabit`) names exactly one base dimension and is
/// recorded directly. A *composite* (`Watt`, a model's `unit Gbps = Gigabit /
/// Second`) is decomposed instead of being skipped -- otherwise it contributes
/// no spelling at all and a product like `Gbps * Hour` has nothing to reduce
/// with. Composites spelled in terms of other composites recurse; `depth`
/// bounds that, since a shadowing declaration could otherwise form a cycle.
fn record_preferred_base_unit(
    name: &str,
    registry: &TypeRegistry,
    candidates: &mut BTreeMap<String, (String, BigRational)>,
    depth: usize,
) {
    const MAX_DEPTH: usize = 8;
    let Some(unit) = registry.units.get(name) else {
        return;
    };
    if unit.dimension.0.len() == 1 {
        let (base, exponent) = unit
            .dimension
            .0
            .iter()
            .next()
            .expect("unit dimension has exactly one component");
        if exponent == &BigRational::one() {
            candidates
                .entry(base.clone())
                .or_insert_with(|| (name.to_owned(), unit.scale.clone()));
            return;
        }
    }
    if depth >= MAX_DEPTH {
        return;
    }
    if !unit.composition.is_empty() {
        // A declared spelling is the best evidence of intent: `unit Gbps =
        // Gigabit / Second` says this model thinks in gigabits, not bytes.
        for constituent in unit.composition.keys() {
            record_preferred_base_unit(constituent, registry, candidates, depth + 1);
        }
        return;
    }
    // No declared spelling (a built-in registered straight from its dimension).
    // Fall back to the registry's canonical unit for each base it spans.
    for base in unit.dimension.0.keys() {
        if let Some(canonical) = registry.canonical_base_unit(base) {
            candidates
                .entry(base.clone())
                .or_insert_with(|| (canonical.name.clone(), canonical.scale.clone()));
        }
    }
}

fn collect_preferred_base_units(
    expression: &Expr,
    registry: &TypeRegistry,
    candidates: &mut BTreeMap<String, (String, BigRational)>,
) {
    let record = |name: &str, candidates: &mut BTreeMap<String, (String, BigRational)>| {
        record_preferred_base_unit(name, registry, candidates, 0);
    };
    match &expression.kind {
        ExprKind::Presentation { value, unit, .. } => {
            collect_preferred_base_units(unit.as_deref().unwrap_or(value), registry, candidates)
        }
        ExprKind::Name { value } => record(value, candidates),
        ExprKind::Number {
            suffix: Some(suffix),
            ..
        } => record(suffix, candidates),
        ExprKind::Unary { value, .. } => collect_preferred_base_units(value, registry, candidates),
        ExprKind::Binary {
            op: BinaryOp::Add | BinaryOp::Sub,
            left,
            right: _,
        } => {
            // Addition retains the left operand's unit convention.
            collect_preferred_base_units(left, registry, candidates)
        }
        ExprKind::Binary { left, right, .. } => {
            collect_preferred_base_units(left, registry, candidates);
            collect_preferred_base_units(right, registry, candidates);
        }
        ExprKind::Call { callee, arguments }
            if matches!(
                callee.path().as_deref(),
                Some("ceil" | "math.ceil" | "floor" | "math.floor" | "min" | "max")
            ) =>
        {
            if let Some(argument) = arguments.first() {
                collect_preferred_base_units(&argument.value, registry, candidates);
            }
        }
        ExprKind::Call { callee, arguments }
            if callee.path().as_deref() == Some("if_then_else") =>
        {
            if let Some(argument) = arguments.get(1) {
                collect_preferred_base_units(&argument.value, registry, candidates);
            }
        }
        ExprKind::Array { items } => {
            if let Some(item) = items.first() {
                collect_preferred_base_units(item, registry, candidates);
            }
        }
        _ => {}
    }
}

fn preferred_unit_factors(
    expression: &Expr,
    registry: &TypeRegistry,
) -> Option<BTreeMap<String, i32>> {
    match &expression.kind {
        ExprKind::Presentation { value, unit, .. } => {
            preferred_unit_factors(unit.as_deref().unwrap_or(value), registry)
        }
        ExprKind::Name { value } if registry.units.contains_key(value) => {
            Some(BTreeMap::from([(value.clone(), 1)]))
        }
        ExprKind::Number {
            suffix: Some(suffix),
            ..
        } if registry.units.contains_key(suffix) => Some(BTreeMap::from([(suffix.clone(), 1)])),
        ExprKind::Number { suffix: None, .. } => Some(BTreeMap::new()),
        ExprKind::Unary { value, .. } => preferred_unit_factors(value, registry),
        ExprKind::Binary {
            op: BinaryOp::Add | BinaryOp::Sub,
            left,
            right,
        } => preferred_unit_factors(left, registry)
            .or_else(|| preferred_unit_factors(right, registry)),
        ExprKind::Binary { op, left, right } if matches!(op, BinaryOp::Mul | BinaryOp::Div) => {
            let left = preferred_unit_factors(left, registry)?;
            let right = preferred_unit_factors(right, registry)?;
            Some(combine_unit_factors(
                left,
                right,
                if *op == BinaryOp::Mul { 1 } else { -1 },
            ))
        }
        ExprKind::Binary {
            op: BinaryOp::Pow,
            left,
            right,
        } => {
            let exponent = numeric_constant(right)?;
            exponent.denom().is_one().then_some(())?;
            let exponent = exponent.numer().to_i32()?;
            let factors = preferred_unit_factors(left, registry)?;
            Some(
                factors
                    .into_iter()
                    .map(|(unit, power)| power.checked_mul(exponent).map(|power| (unit, power)))
                    .collect::<Option<BTreeMap<_, _>>>()?,
            )
        }
        ExprKind::Call { callee, arguments }
            if matches!(
                callee.path().as_deref(),
                Some("ceil" | "math.ceil" | "floor" | "math.floor" | "min" | "max")
            ) =>
        {
            arguments
                .first()
                .and_then(|argument| preferred_unit_factors(&argument.value, registry))
        }
        ExprKind::Call { callee, arguments }
            if callee.path().as_deref() == Some("if_then_else") =>
        {
            arguments
                .get(1)
                .and_then(|argument| preferred_unit_factors(&argument.value, registry))
                .or_else(|| {
                    arguments
                        .get(2)
                        .and_then(|argument| preferred_unit_factors(&argument.value, registry))
                })
        }
        ExprKind::Array { items } => items
            .first()
            .and_then(|item| preferred_unit_factors(item, registry)),
        _ => None,
    }
}

fn combine_unit_factors(
    mut left: BTreeMap<String, i32>,
    right: BTreeMap<String, i32>,
    sign: i32,
) -> BTreeMap<String, i32> {
    for (unit, power) in right {
        *left.entry(unit).or_default() += power * sign;
    }
    left.retain(|_, power| *power != 0);
    left
}

fn format_unit_factors(factors: &BTreeMap<String, i32>) -> String {
    let format_factor = |unit: &str, power: i32| {
        if power == 1 {
            unit.to_owned()
        } else {
            format!("{unit}^{power}")
        }
    };
    let numerator = factors
        .iter()
        .filter(|(_, power)| **power > 0)
        .map(|(unit, power)| format_factor(unit, *power))
        .collect::<Vec<_>>();
    let denominator = factors
        .iter()
        .filter(|(_, power)| **power < 0)
        .map(|(unit, power)| format_factor(unit, -*power))
        .collect::<Vec<_>>();
    match (numerator.is_empty(), denominator.is_empty()) {
        (false, true) => numerator.join(" * "),
        (true, false) => format!("1 / {}", denominator.join(" * ")),
        (false, false) => format!("{} / {}", numerator.join(" * "), denominator.join(" * ")),
        (true, true) => "1".into(),
    }
}

fn format_exact_rational(value: &BigRational) -> String {
    if value.denom().is_one() {
        return value.numer().to_string();
    }

    let mut denominator = value.denom().clone();
    let mut twos = 0_u32;
    let mut fives = 0_u32;
    let two = BigInt::from(2_u8);
    let five = BigInt::from(5_u8);
    while (&denominator % &two).is_zero() {
        denominator /= &two;
        twos += 1;
    }
    while (&denominator % &five).is_zero() {
        denominator /= &five;
        fives += 1;
    }
    if !denominator.is_one() {
        return format!("{}/{}", value.numer(), value.denom());
    }

    let places = twos.max(fives);
    let scale = BigInt::from(10_u8).pow(places);
    let scaled = value.numer() * scale / value.denom();
    let negative = scaled < BigInt::zero();
    let digits = if negative { -scaled } else { scaled }.to_string();
    let places = places as usize;
    let decimal = if digits.len() <= places {
        format!("0.{}{}", "0".repeat(places - digits.len()), digits)
    } else {
        let split = digits.len() - places;
        format!("{}.{}", &digits[..split], &digits[split..])
    };
    if negative {
        format!("-{decimal}")
    } else {
        decimal
    }
}

fn parse_exact_rational_text(value: &str) -> Option<BigRational> {
    if let Some((numerator, denominator)) = value.split_once('/') {
        Some(BigRational::new(
            numerator.parse().ok()?,
            denominator.parse().ok()?,
        ))
    } else {
        parse_rational(value)
    }
}

fn format_metric_bound_range(bounds: &MetricBounds) -> String {
    format_quantity_range(&bounds.lower, &bounds.upper)
}

fn format_quantity_range(lower: &str, upper: &str) -> String {
    let (lower, lower_unit) = split_quantity_display(lower);
    let (upper, upper_unit) = split_quantity_display(upper);
    let lower = truncate_exact_number(lower, 3);
    let upper = truncate_exact_number(upper, 3);
    if lower_unit == upper_unit {
        lower_unit.map_or_else(
            || format!("{lower}–{upper}"),
            |unit| format!("{lower}–{upper} {unit}"),
        )
    } else {
        format!(
            "{}–{}",
            append_quantity_unit(&lower, lower_unit),
            append_quantity_unit(&upper, upper_unit)
        )
    }
}

fn split_quantity_display(value: &str) -> (&str, Option<&str>) {
    value
        .split_once(' ')
        .map_or((value, None), |(number, unit)| (number, Some(unit)))
}

fn append_quantity_unit(number: &str, unit: Option<&str>) -> String {
    unit.map_or_else(|| number.to_owned(), |unit| format!("{number} {unit}"))
}

fn truncate_exact_number(value: &str, places: u32) -> String {
    if let Some((numerator, denominator)) = value.split_once('/')
        && let (Ok(numerator), Ok(denominator)) =
            (numerator.parse::<BigInt>(), denominator.parse::<BigInt>())
        && !denominator.is_zero()
    {
        return format_truncated_rational(&BigRational::new(numerator, denominator), places);
    }
    let Some((whole, fraction)) = value.split_once('.') else {
        return value.to_owned();
    };
    let fraction = fraction
        .chars()
        .take(places as usize)
        .collect::<String>()
        .trim_end_matches('0')
        .to_owned();
    if fraction.is_empty() {
        whole.to_owned()
    } else {
        format!("{whole}.{fraction}")
    }
}

fn format_truncated_rational(value: &BigRational, places: u32) -> String {
    let scale = BigInt::from(10_u8).pow(places);
    let negative = value < &BigRational::zero();
    let magnitude = if negative {
        -value.clone()
    } else {
        value.clone()
    };
    let scaled = magnitude.numer() * &scale / magnitude.denom();
    let whole = &scaled / &scale;
    let fraction = (&scaled % &scale).to_string();
    let fraction = format!("{:0>width$}", fraction, width = places as usize)
        .trim_end_matches('0')
        .to_owned();
    let sign = if negative && (!whole.is_zero() || !fraction.is_empty()) {
        "-"
    } else {
        ""
    };
    if fraction.is_empty() {
        format!("{sign}{whole}")
    } else {
        format!("{sign}{whole}.{fraction}")
    }
}

/// Whether a constant expression denotes a point on an affine scale rather
/// than a magnitude.
///
/// Mirrors the operator rules `infer_binary` enforces, so that formatting
/// agrees with typing: a point plus or minus a magnitude stays a point, and the
/// difference of two points is a magnitude.
fn expression_is_absolute(expression: &Expr, registry: &TypeRegistry) -> bool {
    match &expression.kind {
        ExprKind::Presentation { value, .. } => expression_is_absolute(value, registry),
        ExprKind::Name { value } => registry.units.get(value).is_some_and(Unit::is_absolute),
        ExprKind::Number {
            suffix: Some(suffix),
            ..
        } => registry.units.get(suffix).is_some_and(Unit::is_absolute),
        ExprKind::Unary { value, .. } => expression_is_absolute(value, registry),
        ExprKind::Binary {
            op: BinaryOp::Add,
            left,
            right,
        } => expression_is_absolute(left, registry) || expression_is_absolute(right, registry),
        ExprKind::Binary {
            op: BinaryOp::Sub,
            left,
            right,
        } => expression_is_absolute(left, registry) && !expression_is_absolute(right, registry),
        ExprKind::Call { callee, arguments }
            if matches!(
                callee.path().as_deref(),
                Some(
                    "min" | "max" | "ceil" | "floor" | "math.ceil" | "math.floor" | "if_then_else"
                )
            ) =>
        {
            arguments
                .iter()
                .any(|argument| expression_is_absolute(&argument.value, registry))
        }
        _ => false,
    }
}

fn quantity_suffixes_match(expression: &Expr, expected: &str) -> bool {
    match &expression.kind {
        ExprKind::Number {
            suffix: Some(suffix),
            ..
        } => suffix == expected,
        ExprKind::Presentation { value, unit, .. } => {
            quantity_suffixes_match(unit.as_deref().unwrap_or(value), expected)
        }
        ExprKind::Unary { value, .. } => quantity_suffixes_match(value, expected),
        ExprKind::Binary { left, right, .. } => {
            quantity_suffixes_match(left, expected) || quantity_suffixes_match(right, expected)
        }
        ExprKind::Call { callee, arguments } => {
            quantity_suffixes_match(callee, expected)
                || arguments
                    .iter()
                    .any(|argument| quantity_suffixes_match(&argument.value, expected))
        }
        ExprKind::Array { items } => items
            .iter()
            .any(|item| quantity_suffixes_match(item, expected)),
        ExprKind::Index { value, index, .. } => {
            quantity_suffixes_match(value, expected)
                || index
                    .as_deref()
                    .is_some_and(|index| quantity_suffixes_match(index, expected))
        }
        ExprKind::Lambda { body, .. } => quantity_suffixes_match(body, expected),
        _ => false,
    }
}

fn first_quantity_suffix<'a>(
    expression: &'a Expr,
    registry: &TypeRegistry,
    expected: &Dimension,
) -> Option<&'a str> {
    match &expression.kind {
        ExprKind::Name { value }
            if registry
                .units
                .get(value)
                .is_some_and(|unit| &unit.dimension == expected) =>
        {
            Some(value)
        }
        ExprKind::Number {
            suffix: Some(suffix),
            ..
        } if registry
            .units
            .get(suffix)
            .is_some_and(|unit| &unit.dimension == expected) =>
        {
            Some(suffix)
        }
        ExprKind::Presentation { value, unit, .. } => {
            first_quantity_suffix(unit.as_deref().unwrap_or(value), registry, expected)
        }
        ExprKind::Unary { value, .. } => first_quantity_suffix(value, registry, expected),
        ExprKind::Binary { left, right, .. } => first_quantity_suffix(left, registry, expected)
            .or_else(|| first_quantity_suffix(right, registry, expected)),
        ExprKind::Call { callee, arguments } => first_quantity_suffix(callee, registry, expected)
            .or_else(|| {
                arguments
                    .iter()
                    .find_map(|argument| first_quantity_suffix(&argument.value, registry, expected))
            }),
        ExprKind::Array { items } => items
            .iter()
            .find_map(|item| first_quantity_suffix(item, registry, expected)),
        ExprKind::Index { value, index, .. } => first_quantity_suffix(value, registry, expected)
            .or_else(|| {
                index
                    .as_deref()
                    .and_then(|index| first_quantity_suffix(index, registry, expected))
            }),
        ExprKind::Lambda { body, .. } => first_quantity_suffix(body, registry, expected),
        _ => None,
    }
}

fn combine_condition(existing: Option<&str>, condition: &str, negated: bool) -> String {
    let condition = if negated {
        format!("!({condition})")
    } else {
        condition.into()
    };
    match existing {
        Some(existing) => format!("({existing}) && ({condition})"),
        None => condition,
    }
}

fn visit_block_exprs(block: &Block, visitor: &mut impl FnMut(&Expr)) {
    for statement in &block.statements {
        match statement {
            Stmt::Let { value, .. }
            | Stmt::Requires { value, .. }
            | Stmt::Assign { value, .. }
            | Stmt::Expr { value, .. } => visit_expr(value, visitor),
            Stmt::Yield { value, after, .. } => {
                visit_expr(value, visitor);
                if let Some(after) = after {
                    visit_expr(after, visitor);
                }
            }
            Stmt::For { iterable, body, .. } => {
                visit_expr(iterable, visitor);
                visit_block_exprs(body, visitor);
            }
            Stmt::Repeat { count, body, .. } => {
                visit_expr(count, visitor);
                visit_block_exprs(body, visitor);
            }
            Stmt::If {
                condition,
                then_branch,
                else_branch,
                ..
            } => {
                visit_expr(condition, visitor);
                visit_block_exprs(then_branch, visitor);
                if let Some(else_branch) = else_branch {
                    visit_block_exprs(else_branch, visitor);
                }
            }
            Stmt::Wire { source, target, .. } => {
                visit_expr(source, visitor);
                visit_expr(target, visitor);
            }
            Stmt::Before { first, second, .. } => {
                visit_expr(first, visitor);
                visit_expr(second, visitor);
            }
        }
    }
}

fn lower_effect_block(
    block: &Block,
    runs: &IndexMap<String, RunTemplate>,
    values: &IndexMap<String, Expr>,
) -> Vec<EffectTemplate> {
    block
        .statements
        .iter()
        .filter_map(|statement| match statement {
            Stmt::Let { name, span, .. } if runs.contains_key(name) => Some(EffectTemplate::Run {
                name: name.clone(),
                span: span.clone(),
            }),
            Stmt::Let {
                name, value, span, ..
            } => Some(EffectTemplate::Bind {
                name: name.clone(),
                value: normalize_expr(value, values),
                span: span.clone(),
            }),
            Stmt::Wire {
                source,
                target,
                span,
            } => Some(EffectTemplate::Wire {
                source: normalize_expr(source, values),
                target: normalize_expr(target, values),
                span: span.clone(),
            }),
            Stmt::Assign {
                target,
                value,
                span,
            } => Some(EffectTemplate::Assign {
                target: normalize_expr(target, values),
                value: normalize_expr(value, values),
                span: span.clone(),
            }),
            Stmt::Yield {
                target,
                value,
                after,
                span,
            } => Some(EffectTemplate::Yield {
                target: normalize_expr(target, values),
                value: normalize_expr(value, values),
                after: after.as_ref().map(|after| normalize_expr(after, values)),
                span: span.clone(),
            }),
            Stmt::For {
                binding,
                iterable,
                body,
                span,
            } => Some(EffectTemplate::For {
                binding: binding.clone(),
                iterable: normalize_expr(iterable, values),
                body: lower_effect_block(body, runs, values),
                span: span.clone(),
            }),
            Stmt::Repeat { count, body, span } => Some(EffectTemplate::Repeat {
                count: normalize_expr(count, values),
                body: lower_effect_block(body, runs, values),
                span: span.clone(),
            }),
            Stmt::If {
                condition,
                then_branch,
                else_branch,
                span,
            } => Some(EffectTemplate::Branch {
                condition: normalize_expr(condition, values),
                then_body: lower_effect_block(then_branch, runs, values),
                else_body: else_branch
                    .as_ref()
                    .map(|branch| lower_effect_block(branch, runs, values))
                    .unwrap_or_default(),
                span: span.clone(),
            }),
            Stmt::Before {
                first,
                second,
                span,
            } => Some(EffectTemplate::Before {
                first: first.display(),
                second: second.display(),
                span: span.clone(),
            }),
            Stmt::Expr { value, span } => {
                let ExprKind::Call { callee, arguments } = &value.kind else {
                    return None;
                };
                let ExprKind::Member {
                    value: receiver,
                    member,
                } = &callee.kind
                else {
                    return None;
                };
                if member != "append" || arguments.len() != 1 {
                    return None;
                }
                // An appended accumulator is a real data dependency even
                // though its compile-time value becomes symbolic. Lower the
                // append as an effect assignment so the graph projection can
                // trace a later wire back through a loop binding to the run
                // export that produced the collection's elements.
                Some(EffectTemplate::Assign {
                    target: normalize_expr(receiver, values),
                    value: normalize_expr(&arguments[0].value, values),
                    span: span.clone(),
                })
            }
            _ => None,
        })
        .collect()
}

/// Resolve local ownership aliases for the static flow projection. Repeats
/// are visited once: their first-iteration input and post-iteration output are
/// enough to show the physical stage DAG without drawing a causal cycle for
/// every subsequent iteration. The exact repeated edges remain in the effect
/// schedule rather than being lost.
fn effect_wire_sources(effects: &[EffectTemplate]) -> HashMap<String, String> {
    fn resolve(value: &Expr, aliases: &HashMap<String, String>) -> String {
        let display = value.path().unwrap_or_else(|| value.display());
        aliases.get(&display).cloned().unwrap_or(display)
    }

    fn visit(
        effects: &[EffectTemplate],
        aliases: &mut HashMap<String, String>,
        sources: &mut HashMap<String, String>,
    ) {
        for effect in effects {
            match effect {
                EffectTemplate::Bind { name, value, .. } => {
                    aliases.insert(name.clone(), resolve(value, aliases));
                }
                EffectTemplate::Assign { target, value, .. } => {
                    if let Some(target) = target.path() {
                        aliases.insert(target, resolve(value, aliases));
                    }
                }
                EffectTemplate::Wire { source, target, .. } => {
                    if let Some(target) = target.path() {
                        sources.insert(target, resolve(source, aliases));
                    }
                }
                EffectTemplate::For {
                    binding,
                    iterable,
                    body,
                    ..
                } => {
                    let mut nested = aliases.clone();
                    nested.insert(binding.clone(), resolve(iterable, aliases));
                    visit(body, &mut nested, sources);
                    for name in aliases.keys().cloned().collect::<Vec<_>>() {
                        if let Some(value) = nested.get(&name) {
                            aliases.insert(name, value.clone());
                        }
                    }
                }
                EffectTemplate::Repeat { body, .. } => visit(body, aliases, sources),
                EffectTemplate::Branch {
                    then_body,
                    else_body,
                    ..
                } => {
                    let mut then_aliases = aliases.clone();
                    let mut else_aliases = aliases.clone();
                    visit(then_body, &mut then_aliases, sources);
                    visit(else_body, &mut else_aliases, sources);
                    for name in aliases.keys().cloned().collect::<Vec<_>>() {
                        if then_aliases.get(&name) == else_aliases.get(&name)
                            && let Some(value) = then_aliases.get(&name)
                        {
                            aliases.insert(name, value.clone());
                        }
                    }
                }
                EffectTemplate::Run { .. }
                | EffectTemplate::Yield { .. }
                | EffectTemplate::Before { .. } => {}
            }
        }
    }

    let mut aliases = HashMap::new();
    let mut sources = HashMap::new();
    visit(effects, &mut aliases, &mut sources);
    sources
}

/// Whether an expression can do anything when evaluated as a statement.
///
/// Calls are the only construct with effects (`x.append(v)` lowers to an
/// effect assignment; runs and pure-function applications happen here too).
/// Every other kind just builds a value, so as a statement it is dead code —
/// and silently accepting it lets a dropped unit suffix like the trailing
/// `Meter` in `(1000 * 3) Meter` masquerade as a valid program.
fn expression_can_have_effect(expression: &Expr) -> bool {
    let mut found = false;
    visit_expr(expression, &mut |inner| {
        if matches!(inner.kind, ExprKind::Call { .. }) {
            found = true;
        }
    });
    found
}

fn expression_requests_approximation(expression: &Expr) -> bool {
    let mut approximate = false;
    visit_expr(expression, &mut |expression| {
        approximate |= matches!(
            expression.kind,
            ExprKind::Presentation {
                approximate: true,
                ..
            }
        );
    });
    approximate
}

fn visit_expr(expression: &Expr, visitor: &mut impl FnMut(&Expr)) {
    visitor(expression);
    match &expression.kind {
        ExprKind::NamedTuple { fields } => {
            for field in fields {
                visit_expr(&field.value, visitor);
            }
        }
        ExprKind::Presentation { value, .. }
        | ExprKind::Unary { value, .. }
        | ExprKind::Member { value, .. } => visit_expr(value, visitor),
        ExprKind::Binary { left, right, .. } => {
            visit_expr(left, visitor);
            visit_expr(right, visitor);
        }
        ExprKind::Index { value, index, .. } => {
            visit_expr(value, visitor);
            if let Some(index) = index {
                visit_expr(index, visitor);
            }
        }
        ExprKind::Call { callee, arguments } => {
            visit_expr(callee, visitor);
            for argument in arguments {
                visit_expr(&argument.value, visitor);
            }
        }
        ExprKind::Lambda { body, .. } => visit_expr(body, visitor),
        ExprKind::Array { items } => {
            for item in items {
                visit_expr(item, visitor);
            }
        }
        _ => {}
    }
}

fn reaches(
    origin: &str,
    current: &str,
    graph: &HashMap<String, BTreeSet<String>>,
    visiting: &mut HashSet<String>,
) -> bool {
    if !visiting.insert(current.into()) {
        return current == origin;
    }
    let result = graph.get(current).is_some_and(|next| {
        next.iter()
            .any(|next| next == origin || reaches(origin, next, graph, visiting))
    });
    visiting.remove(current);
    result
}

fn is_ordered(first: &str, second: &str, graph: &HashMap<String, BTreeSet<String>>) -> bool {
    fn visit(
        current: &str,
        target: &str,
        graph: &HashMap<String, BTreeSet<String>>,
        seen: &mut HashSet<String>,
    ) -> bool {
        if !seen.insert(current.to_owned()) {
            return false;
        }
        graph.get(current).is_some_and(|next| {
            next.contains(target) || next.iter().any(|next| visit(next, target, graph, seen))
        })
    }
    visit(first, second, graph, &mut HashSet::new())
}

fn child_port_mutates(
    process: &ProcessModel,
    summaries: &IndexMap<String, ProcessSummary>,
    run: &str,
    port: &str,
) -> bool {
    let Some(run) = process.runs.get(run) else {
        return true;
    };
    summaries.get(&run.process).is_none_or(|summary| {
        summary
            .mutations
            .iter()
            .any(|mutation| mutation.artifact == port)
    })
}

fn is_snake_case(name: &str) -> bool {
    !name.is_empty()
        && !name.chars().any(char::is_uppercase)
        && name.chars().all(|character| {
            character.is_ascii_lowercase() || character.is_ascii_digit() || character == '_'
        })
}

impl fmt::Display for Compilation {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.render_human())
    }
}

#[cfg(test)]
mod tests {
    use crate::report::Completeness as Status;
    use std::fs;
    use std::path::Path;

    use tempfile::tempdir;

    use super::*;

    #[test]
    fn numeric_refinements_check_concrete_and_symbolic_process_boundaries() {
        let declarations = "unit PositiveScalar = Scalar(x) { requires x > 0 }\nunit Fraction = PositiveScalar(x) { requires x <= 1 }\nunit PositiveLength = Length(x) { requires x > 0 Meter }\n";
        let good = check_source(&format!(
            "{declarations}\nprocess Test {{ config a: Fraction = 0.5\n export length: PositiveLength\n () => {{ length = 2 Millimeter }} }}"
        ));
        assert!(
            good.report.diagnostics.is_empty(),
            "{}",
            good.render_human()
        );
        assert!(
            good.report.constraints.is_empty(),
            "{}",
            good.render_human()
        );
        for value in ["0", "-1", "2"] {
            let bad = check_source(&format!(
                "{declarations}\nprocess Test {{ config a: Fraction = {value}\n () => {{ }} }}"
            ));
            assert!(
                bad.report.diagnostics.iter().any(|d| d.code == "E0901"),
                "{}",
                bad.render_human()
            );
        }
        let symbolic = check_source(&format!(
            "{declarations}\nprocess Test {{ config a: Fraction\n () => {{ }} }}"
        ));
        assert!(
            symbolic.report.diagnostics.is_empty(),
            "{}",
            symbolic.render_human()
        );
        assert_eq!(
            symbolic.report.constraints.len(),
            2,
            "{}",
            symbolic.render_human()
        );
    }

    #[test]
    fn numeric_refinements_do_not_leak_through_arithmetic_and_check_mutation() {
        let source = "unit Positive = Scalar(x) { requires x > 0 }\nprocess Test { () => { let a: Positive = 2\n let b: Positive = 3\n let difference = a - b\n requires difference == -1\n } }";
        let good = check_source(source);
        assert!(
            good.report.diagnostics.is_empty(),
            "{}",
            good.render_human()
        );
        let bad = check_source(&source.replace("let difference =", "let difference: Positive ="));
        assert!(
            bad.report.diagnostics.iter().any(|d| d.code == "E0603"),
            "{}",
            bad.render_human()
        );
        let mutation = check_source(
            "unit Positive = Scalar(x) { requires x > 0 }\nprocess Test { () => { let mut x: Positive = 2\n x = -1 } }",
        );
        assert!(
            mutation
                .report
                .diagnostics
                .iter()
                .any(|d| d.code == "E0603"),
            "{}",
            mutation.render_human()
        );
    }

    #[test]
    fn numeric_refinements_validate_bases_predicates_and_binders() {
        for declaration in [
            "unit Scalar = Scalar(x) { requires x > 0 }",
            "unit Bad = Bool(x) { requires x }",
            "unit Bad = Scalar(x, y) { requires x > 0 }",
            "unit Bad = Scalar(x) { let y = x }",
            "unit Bad = Scalar(x) { requires x + 1 }",
            "unit Bad = Scalar(x) { requires absent > 0 }",
        ] {
            let bad = check_source(declaration);
            assert!(
                !bad.report.diagnostics.is_empty(),
                "{declaration}: {}",
                bad.render_human()
            );
        }
    }

    #[test]
    fn numeric_refinements_check_functions_artifacts_and_integer_values() {
        let bad = check_source(
            "unit PositiveInteger = Integer(x) { requires x > 0 }\nfn f(x: PositiveInteger) -> (y: PositiveInteger) { y = x }\nprocess Test { () => { let value = f(-2) } }",
        );
        assert!(
            bad.report.diagnostics.iter().any(|d| d.code == "E0603"),
            "{}",
            bad.render_human()
        );
        let bad = check_source(
            "unit PositiveInteger = Integer(x) { requires x > 0 }\nprocess Test { () => { let value: PositiveInteger = 1.5 } }",
        );
        assert!(
            bad.report.diagnostics.iter().any(|d| d.code == "E0603"),
            "{}",
            bad.render_human()
        );
        let bad = check_source(
            "unit Positive = Scalar(x) { requires x > 0 }\nartifact Sample { mass: Positive }\nprocess Test { () => { let sample = Sample(mass: -1) } }",
        );
        assert!(
            bad.report.diagnostics.iter().any(|d| d.code == "E0603"),
            "{}",
            bad.render_human()
        );
    }

    #[test]
    fn numeric_refinements_propagate_child_function_and_array_constraints() {
        let source = "unit Positive = Scalar(x) { requires x > 0 }\nprocess Child { config amount: Positive\n (input: Positive) => {} }\nprocess Root { () => { let child = Child(amount: 2)(input: -1) } }";
        let bad = check_source(source);
        assert!(
            bad.report
                .diagnostics
                .iter()
                .any(|d| d.code == "E0901" && d.message.contains("Root.child")),
            "{}",
            bad.render_human()
        );
        let bad = check_source(
            "unit Positive = Scalar(x) { requires x > 0 }\nprocess Specification { config amount: Positive = -1 }",
        );
        assert!(
            bad.report.diagnostics.iter().any(|d| d.code == "E0901"),
            "{}",
            bad.render_human()
        );
        let bad = check_source(
            "unit Positive = Scalar(x) { requires x > 0 }\nfn f(x: Scalar) -> (y: Scalar) { let v: Positive = x - 5\n y = x }\nprocess Root { () => { let result = f(2) } }",
        );
        assert!(
            bad.report.diagnostics.iter().any(|d| d.code == "E0603"),
            "{}",
            bad.render_human()
        );
        let bad = check_source(
            "unit Positive = Scalar(x) { requires x > 0 }\nprocess Root { () => { let values: Positive[] = [1, -2, 3] } }",
        );
        assert!(
            bad.report.diagnostics.iter().any(|d| d.code == "E0603"),
            "{}",
            bad.render_human()
        );
    }

    #[test]
    fn numeric_refinements_check_collection_insertion_stream_items_and_fields() {
        for body in [
            "let mut values: Positive[] = [1]\n values.append(-1)",
            "let mut values: Positive[] = [1]\n values[0] = -1",
            "let sample = Sample(mass: 1)\n sample.mass = -1",
        ] {
            let bad = check_source(&format!(
                "unit Positive = Scalar(x) {{ requires x > 0 }}\nartifact Sample {{ mass: Positive }}\nprocess Root {{ () => {{ {body} }} }}"
            ));
            assert!(
                bad.report.diagnostics.iter().any(|d| d.code == "E0603"),
                "{}",
                bad.render_human()
            );
        }
        let bad = check_source(
            "unit Positive = Scalar(x) { requires x > 0 }\nprocess Root { export values: Stream[Positive]\n () => { yield values = -1 } }",
        );
        assert!(
            bad.report.diagnostics.iter().any(|d| d.code == "E0603"),
            "{}",
            bad.render_human()
        );
    }

    #[test]
    fn numeric_refinements_reject_conflicting_inherited_contracts() {
        let bad = check_source(
            "unit Positive = Scalar(x) { requires x > 0 }\nunit Negative = Scalar(x) { requires x < 0 }\nprocess A { config amount: Positive }\nprocess B { config amount: Negative }\nprocess C: A, B { () => {} }",
        );
        assert!(
            bad.report.diagnostics.iter().any(|d| d.code == "E0503"),
            "{}",
            bad.render_human()
        );
    }

    #[test]
    fn numeric_refinements_support_concrete_mutating_function_loops() {
        let bad = check_source(
            "unit Positive = Scalar(x) { requires x > 0 }\nfn f() -> (out: Scalar) { let mut value: Positive = 2\n for item in [1, 2, 3] { value = value - 1 }\n out = value }\nprocess Root { () => { let result = f() } }",
        );
        assert!(
            bad.report.diagnostics.iter().any(|d| d.code == "E0603"),
            "{}",
            bad.render_human()
        );
        let unsupported = check_source(
            "unit Positive = Scalar(x) { requires x > 0 }\nfn f(items: Scalar[]) -> (out: Scalar) { let mut value: Positive = 2\n for item in items { value = value - item }\n out = value }\nprocess Root { () => { let result = f([1, 2, 3]) } }",
        );
        assert!(
            unsupported
                .report
                .diagnostics
                .iter()
                .any(|d| d.code == "E0204"),
            "{}",
            unsupported.render_human()
        );
    }

    #[test]
    fn numeric_refinements_do_not_silently_erase_unsupported_contracts() {
        let unsupported = check_source(
            "unit Positive = Scalar(x) { requires x > 0 }\ntrait Measure { fn f(x: Positive) -> (out: Scalar) }\n",
        );
        assert!(
            unsupported
                .report
                .diagnostics
                .iter()
                .any(|d| d.code == "E0204"),
            "{}",
            unsupported.render_human()
        );
        let unsupported = check_source(
            "unit Positive = Scalar(x) { requires x > 0 }\nfn f(x: Positive) -> (out: Scalar) { out = x }\nprocess Root { config x: Scalar\n config y: Scalar = f(x).out\n () => {} }",
        );
        assert!(
            unsupported
                .report
                .diagnostics
                .iter()
                .any(|d| d.code == "E0204"),
            "{}",
            unsupported.render_human()
        );
        let good = check_source(
            "unit Positive = Scalar(x) { requires x > 0 }\nfn f(x: Positive) -> (out: Scalar) { out = x }\nprocess Root { config y: Scalar = f(2).out\n () => {} }",
        );
        assert!(
            good.report.diagnostics.is_empty(),
            "{}",
            good.render_human()
        );
    }

    #[test]
    fn numeric_refinements_keep_imported_guard_diagnostics_at_the_call_site() {
        let directory = tempdir().unwrap();
        let declarations = format!(
            "{}\nunit Positive = Scalar(x) {{ requires x > 0 }}\nartifact Sample {{ value: Positive {{ -1 }} }}\nprocess Contract {{ config amount: Positive }}",
            "# source padding\n".repeat(100)
        );
        fs::write(directory.path().join("refinements.req"), declarations).unwrap();
        let source = "import \"refinements.req\" (Sample, Contract)\nprocess Root: Contract { () => { let sample = Sample() } }\n";
        fs::write(directory.path().join("main.req"), source).unwrap();
        let result = Compiler::new(CompilerOptions::default())
            .check_path(&directory.path().join("main.req"))
            .unwrap();
        let errors = result
            .report
            .diagnostics
            .iter()
            .filter(|d| d.severity == Severity::Error)
            .collect::<Vec<_>>();
        assert!(!errors.is_empty(), "{}", result.render_human());
        assert!(
            errors
                .iter()
                .all(|d| d.span.path == "main.req" && d.span.end <= source.len()),
            "{}",
            result.render_human()
        );
    }

    fn diagnostic_codes(compilation: &Compilation) -> Vec<&str> {
        compilation
            .report
            .diagnostics
            .iter()
            .map(|diagnostic| diagnostic.code.as_str())
            .collect()
    }

    fn monitored(compilation: &Compilation, path: &str) -> String {
        compilation
            .report
            .monitored_metrics
            .iter()
            .find(|metric| metric.path == path)
            .unwrap_or_else(|| panic!("{path} was not monitored\n{}", compilation.render_human()))
            .value
            .clone()
    }

    /// Explicit unit declarations override built-ins without diagnostics.
    #[test]
    fn user_unit_declarations_shadow_builtins_without_a_diagnostic() {
        let compilation = check_source(
            r#"
                unit Hour = Second * 7200
                unit Gigabyte = Second

                process Timing {
                    export duration: Time aggregate sum
                    monitor duration
                    () => { duration = 1 Hour }
                }
            "#,
        );
        assert!(
            compilation.report.diagnostics.is_empty(),
            "shadowing a builtin must be silent, got {:?}\n{}",
            diagnostic_codes(&compilation),
            compilation.render_human()
        );
        // The author's definition wins outright, including a deliberately
        // absurd one that changes the shadowed unit's dimension.
        assert_eq!(monitored(&compilation, "Timing.duration"), "1 Hour");
        assert_eq!(
            compilation.registry.units["Hour"].scale,
            BigRational::from_integer(7_200.into())
        );
        assert_eq!(
            compilation.registry.units["Gigabyte"].dimension,
            compilation.registry.units["Second"].dimension
        );

        // Declaring the same unit in two loaded modules is still an error;
        // only *built-in* names may be shadowed.
        let directory = tempdir().unwrap();
        let library = directory.path().join("library.req");
        let main = directory.path().join("main.req");
        fs::write(&library, "unit Furlong = Meter * 201\n").unwrap();
        fs::write(
            &main,
            "import \"library.req\" (Furlong)\nunit Furlong = Meter * 202\n",
        )
        .unwrap();
        let duplicate = Compiler::new(CompilerOptions::default())
            .check_path(&main)
            .unwrap();
        assert!(
            diagnostic_codes(&duplicate).contains(&"E0201"),
            "{}",
            duplicate.render_human()
        );
    }

    #[test]
    fn builtin_units_are_available_without_declaration() {
        let compilation = check_source(
            r#"
                process Link {
                    export moved: Gigabyte aggregate sum
                    monitor moved as Terabyte
                    () => { moved = 2500 Gigabyte }
                }
            "#,
        );
        assert_eq!(
            compilation.report.status,
            Completeness::Complete,
            "{}",
            compilation.render_human()
        );
        assert_eq!(monitored(&compilation, "Link.moved"), "2.5 Terabyte");
    }

    /// `42 Celsius as Fahrenheit` is exactly 107.6. The whole point of the
    /// affine schema is that this is not a rounded float.
    #[test]
    fn absolute_temperatures_convert_through_the_full_affine_transform() {
        let compilation = check_source(
            r#"
                process Oven {
                    export setpoint: Celsius aggregate max
                    monitor setpoint as Fahrenheit
                    () => { setpoint = 42 Celsius }
                }
                process Freezing {
                    export point: Fahrenheit aggregate max
                    monitor point as Kelvin
                    () => { point = 32 Fahrenheit }
                }
                process Boiling {
                    export point: Fahrenheit aggregate max
                    monitor point as Celsius
                    () => { point = 212 Fahrenheit }
                }
            "#,
        );
        assert!(
            compilation.report.diagnostics.is_empty(),
            "{}",
            compilation.render_human()
        );
        assert_eq!(monitored(&compilation, "Oven.setpoint"), "107.6 Fahrenheit");
        assert_eq!(monitored(&compilation, "Freezing.point"), "273.15 Kelvin");
        assert_eq!(monitored(&compilation, "Boiling.point"), "100 Celsius");
    }

    /// A difference of two points is a magnitude, and must be reported as one:
    /// rendering five degrees of difference as Celsius would read -268.15.
    #[test]
    fn differencing_two_absolute_temperatures_yields_a_relative_quantity() {
        let compilation = check_source(
            r#"
                process Delta {
                    export rise: Kelvin aggregate sum
                    monitor rise
                    () => { rise = 25 Celsius - 20 Celsius }
                }
                process Offset {
                    export warmed: Celsius aggregate max
                    monitor warmed
                    () => { warmed = 20 Celsius + 5 Kelvin }
                }
            "#,
        );
        assert!(
            compilation.report.diagnostics.is_empty(),
            "{}",
            compilation.render_human()
        );
        assert_eq!(monitored(&compilation, "Delta.rise"), "5 Kelvin");
        // A point offset by a magnitude stays a point on its own scale.
        assert_eq!(monitored(&compilation, "Offset.warmed"), "25 Celsius");
    }

    /// A dimensionless value displayed as a dimensioned unit is the one case
    /// where the author probably meant to attach a dimension, so `E0512` names
    /// the operation that does it. A real cross-dimension mismatch must not get
    /// the same hint -- there `* 1 Second` would be wrong advice.
    #[test]
    fn display_unit_mismatch_hints_only_when_the_value_is_dimensionless() {
        let scalar = check_source(
            r#"
                process Bare {
                    export ratio: Scalar aggregate sum
                    monitor ratio as Meter
                    () => { ratio = 1000 * 3 }
                }
            "#,
        );
        let scalar_message = scalar
            .report
            .diagnostics
            .iter()
            .find(|diagnostic| diagnostic.code == "E0512")
            .map(|diagnostic| diagnostic.message.clone())
            .expect("dimensionless display mismatch reports E0512");
        assert!(
            scalar_message.contains("* 1 Meter"),
            "expected an attach-a-dimension hint, got: {scalar_message}"
        );

        let mismatch = check_source(
            r#"
                process Wrong {
                    export span: Length aggregate sum
                    monitor span as Second
                    () => { span = 5 Meter }
                }
            "#,
        );
        let mismatch_message = mismatch
            .report
            .diagnostics
            .iter()
            .find(|diagnostic| diagnostic.code == "E0512")
            .map(|diagnostic| diagnostic.message.clone())
            .expect("cross-dimension display mismatch reports E0512");
        assert!(
            !mismatch_message.contains("* 1"),
            "a real dimension mismatch must not suggest multiplying: {mismatch_message}"
        );
    }

    /// A statement that only builds a value cannot do anything. Accepting one
    /// silently is how `(1000 * 3) Meter` parses as two statements and quietly
    /// drops the unit.
    #[test]
    fn expression_statements_without_effects_are_rejected() {
        let dead = check_source(
            r#"
                process Dropped {
                    export total: Scalar aggregate sum
                    () => {
                        let value = (1000 * 3)
                        Meter
                        total = value
                    }
                }
            "#,
        );
        assert!(
            diagnostic_codes(&dead).contains(&"E0439"),
            "{}",
            dead.render_human()
        );

        // A call can have an effect, so it stays legal as a statement.
        let appended = check_source(
            r#"
                process Kept {
                    export total: Scalar aggregate sum
                    () => {
                        let mut items: Scalar[] = []
                        items.append(1)
                        total = items.length()
                    }
                }
            "#,
        );
        assert!(
            !diagnostic_codes(&appended).contains(&"E0439"),
            "{}",
            appended.render_human()
        );
    }

    /// A named composite carries a spelling for each base dimension it spans,
    /// so a product involving one can still be reduced. Without this, `Gbps`
    /// contributes nothing and `1 Gbps * 40 Hour` prints as `40 Gbps * Hour`.
    #[test]
    fn composite_units_still_reduce_in_products() {
        let compilation = check_source(
            r#"
                unit Gbps = Gigabit / Second
                unit MillimeterPerSecond = Millimeter / Second
                process Transfer {
                    export moved: Information aggregate sum
                    monitor moved
                    () => { moved = 1 Gbps * 40 Hour }
                }
                process Builtin {
                    export moved: Information aggregate sum
                    monitor moved
                    () => { moved = 1 GigabitPerSecond * 40 Hour }
                }
                process Rate {
                    export distance: Length aggregate sum
                    monitor distance
                    () => { distance = 10 MillimeterPerSecond * 3 Second }
                }
            "#,
        );
        assert!(
            compilation.report.diagnostics.is_empty(),
            "{}",
            compilation.render_human()
        );
        // The declared spelling wins: this model thinks in gigabits, not bytes.
        assert_eq!(monitored(&compilation, "Transfer.moved"), "144000 Gigabit");
        assert_eq!(monitored(&compilation, "Builtin.moved"), "144000 Gigabit");
        assert_eq!(monitored(&compilation, "Rate.distance"), "30 Millimeter");
    }

    /// A reduced dimension is reported under its standard name when the
    /// author's base units are the coherent SI ones -- and left expanded when
    /// they are not, so a cgs model is not rewritten into awkward Joules.
    #[test]
    fn coherent_si_products_collapse_to_their_derived_unit() {
        let compilation = check_source(
            r#"
                process Work {
                    export energy: Energy aggregate sum
                    monitor energy
                    () => { energy = 1 Newton * 5 Meter }
                }
                process Spun {
                    export energy: Energy aggregate sum
                    monitor energy
                    () => { energy = 1 Watt * 40 Hour }
                }
                process Draw {
                    export load: Power aggregate sum
                    monitor load
                    () => { load = 1 Volt * 2 Ampere }
                }
                process Cgs {
                    export energy: Energy aggregate sum
                    monitor energy
                    () => { energy = 1 Gram * 1 Centimeter ^ 2 / 1 Second ^ 2 }
                }
            "#,
        );
        assert!(
            compilation.report.diagnostics.is_empty(),
            "{}",
            compilation.render_human()
        );
        assert_eq!(monitored(&compilation, "Work.energy"), "5 Joule");
        assert_eq!(monitored(&compilation, "Spun.energy"), "144000 Joule");
        assert_eq!(monitored(&compilation, "Draw.load"), "2 Watt");
        // Scale is 1e-7 here, so the author's own spelling is preserved.
        assert_eq!(
            monitored(&compilation, "Cgs.energy"),
            "1 Centimeter^2 * Gram / Second^2"
        );
    }

    /// A leading sign on a literal belongs to the literal, so sub-zero
    /// readings stay writable. Negating a *computed* point is still nonsense —
    /// see `absolute_temperatures_reject_meaningless_arithmetic`.
    #[test]
    fn negative_literals_are_ordinary_absolute_temperatures() {
        let compilation = check_source(
            r#"
                process Cold {
                    export point: Celsius aggregate max
                    monitor point
                    () => { point = -40 Celsius }
                }
                process Coincide {
                    export same: Fahrenheit aggregate max
                    monitor same as Fahrenheit
                    () => { same = -40 Celsius }
                }
                process AbsoluteZero {
                    export zero: Celsius aggregate max
                    monitor zero as Kelvin
                    () => { zero = -273.15 Celsius }
                }
            "#,
        );
        assert!(
            compilation.report.diagnostics.is_empty(),
            "{}",
            compilation.render_human()
        );
        assert_eq!(monitored(&compilation, "Cold.point"), "-40 Celsius");
        // -40 is the one reading where the two scales coincide.
        assert_eq!(monitored(&compilation, "Coincide.same"), "-40 Fahrenheit");
        // The offset must be applied to the *signed* magnitude, not negated
        // after conversion, which would give -313.15 K.
        assert_eq!(monitored(&compilation, "AbsoluteZero.zero"), "0 Kelvin");
    }

    #[test]
    fn absolute_temperatures_reject_meaningless_arithmetic() {
        let cases = [
            ("20 Celsius + 5 Celsius", "E0837"),
            ("2 * 20 Celsius", "E0836"),
            ("20 Celsius * 2", "E0836"),
            ("20 Celsius / 2", "E0836"),
            ("0 Kelvin - 20 Celsius", "E0836"),
            ("20 Celsius ^ 2", "E0836"),
        ];
        for (expression, expected) in cases {
            let compilation = check_source(&format!(
                r#"
                    process Bad {{
                        export value: Kelvin aggregate sum
                        () => {{ value = {expression} }}
                    }}
                "#
            ));
            assert!(
                diagnostic_codes(&compilation).contains(&expected),
                "{expression} should report {expected}\n{}",
                compilation.render_human()
            );
        }

        // Unary minus on a point is nonsense too.
        let negated = check_source(
            r#"
                process Bad {
                    export value: Kelvin aggregate sum
                    () => { value = 0 Kelvin - (20 Celsius) }
                }
            "#,
        );
        assert!(
            diagnostic_codes(&negated).contains(&"E0836"),
            "{}",
            negated.render_human()
        );
    }

    #[test]
    fn absolute_temperatures_order_against_each_other_but_not_against_magnitudes() {
        let ordered = check_source(
            r#"
                process Check {
                    export value: Kelvin aggregate sum
                    () => {
                        value = 0 Kelvin
                        requires 20 Celsius < 30 Celsius
                        requires 20 Celsius > 60 Fahrenheit
                        requires 5 Kelvin < 10 Kelvin
                    }
                }
            "#,
        );
        assert!(
            !diagnostic_codes(&ordered).contains(&"E0836"),
            "comparing two points is fine\n{}",
            ordered.render_human()
        );

        let mixed = check_source(
            r#"
                process Check {
                    export value: Kelvin aggregate sum
                    () => {
                        value = 0 Kelvin
                        requires 20 Celsius > 5 Kelvin
                    }
                }
            "#,
        );
        assert!(
            diagnostic_codes(&mixed).contains(&"E0836"),
            "{}",
            mixed.render_human()
        );
    }

    #[test]
    fn absolute_units_are_rejected_inside_compound_unit_types() {
        let compilation = check_source(
            r#"
                process Ramp {
                    export slope: Celsius / Second aggregate sum
                    () => { slope = 1 Kelvin / 1 Second }
                }
                process Capacity {
                    export heat: Joule / Celsius aggregate sum
                    () => { heat = 1 Joule / 1 Kelvin }
                }
            "#,
        );
        let reasons = compilation
            .report
            .diagnostics
            .iter()
            .filter(|diagnostic| diagnostic.code == "E0505")
            .map(|diagnostic| diagnostic.message.clone())
            .collect::<Vec<_>>();
        assert_eq!(reasons.len(), 2, "{}", compilation.render_human());
        for reason in &reasons {
            assert!(reason.contains("Celsius"), "{reason}");
            assert!(reason.contains("Kelvin"), "{reason}");
        }
    }

    /// Totalling thermometer readings is meaningless; ordering them is not.
    #[test]
    fn aggregating_absolute_exports_allows_extremes_but_not_totals() {
        for rule in ["sum", "makespan"] {
            let compilation = check_source(&format!(
                r#"
                    process Reading {{
                        export temperature: Celsius aggregate {rule}
                        () => {{ temperature = 20 Celsius }}
                    }}
                "#
            ));
            let rejected = compilation.report.diagnostics.iter().any(|diagnostic| {
                diagnostic.code == "E0509" && diagnostic.message.contains("absolute")
            });
            assert!(
                rejected,
                "aggregate {rule} over an absolute export must be rejected\n{}",
                compilation.render_human()
            );
        }

        for rule in ["max", "min"] {
            let compilation = check_source(&format!(
                r#"
                    process Reading {{
                        export temperature: Celsius aggregate {rule}
                        monitor temperature
                        () => {{ temperature = 20 Celsius }}
                    }}
                "#
            ));
            assert!(
                compilation.report.diagnostics.is_empty(),
                "aggregate {rule} over an absolute export is legal\n{}",
                compilation.render_human()
            );
            assert_eq!(monitored(&compilation, "Reading.temperature"), "20 Celsius");
        }
    }

    /// `zero_expression` seeds aggregate defaults. `0 Celsius` is 273.15 K, so
    /// emitting it would silently shift any total that used it.
    #[test]
    fn synthesized_aggregate_zeros_are_never_absolute() {
        // A composite whose child contributes nothing falls back to the
        // synthesized zero for the export's own type.
        let compilation = check_source(
            r#"
                process Child {
                    export other: Kelvin aggregate max
                    () => { other = 1 Kelvin }
                }
                process Parent {
                    export temperature: Celsius aggregate max
                    () => { let child = Child()() }
                }
            "#,
        );
        let predicate = compilation
            .report
            .constraints
            .iter()
            .find(|constraint| constraint.path.contains("aggregate[temperature]"))
            .map(|constraint| constraint.predicate.clone())
            .unwrap_or_else(|| panic!("no aggregate constraint\n{}", compilation.render_human()));
        assert_eq!(predicate, "temperature == 0 Kelvin");
        assert!(!predicate.contains("Celsius"), "{predicate}");
    }

    fn check_source(source: &str) -> Compilation {
        let directory = tempdir().unwrap();
        let path = directory.path().join("main.req");
        fs::write(&path, source).unwrap();
        Compiler::new(CompilerOptions::default())
            .check_path(Path::new(&path))
            .unwrap()
    }

    #[test]
    fn named_tuples_copy_through_config_invocation_and_exports() {
        let result = check_source(
            r#"
process Child {
    config settings: (scale: Scalar, dimensions: (width: Length, height: Length))
    export summary: (distance: Length, good: Bool)
    (measurement: (distance: Length, good: Bool)) => {
        summary = (good: measurement.good, distance: measurement.distance * settings.scale)
    }
}
process Main {
    export total: Length
    () => {
        let settings = (dimensions: (height: 2 Millimeter, width: 1 Millimeter), scale: 2)
        let measurement: (good: Bool, distance: Length) = (distance: 3 Meter, good: true)
        let first = Child(settings: settings)(measurement: measurement)
        let second = Child(settings: settings)(measurement: measurement)
        total = first.summary.distance + second.summary.distance
        requires first.summary.distance == 6 Meter
    }
}
"#,
        );
        assert_eq!(
            result.report.status,
            Status::Complete,
            "{}",
            result.render_human()
        );
        assert!(
            result.report.constraints.is_empty(),
            "{}",
            result.render_human()
        );
    }

    #[test]
    fn named_tuple_types_and_literals_reject_owned_or_resource_fields() {
        for declaration in ["artifact A {}", "resource A { capacity: 2 }"] {
            let result = check_source(&format!(
                "{declaration}\nprocess Main {{ config settings: (nested: (bad: A)) }}"
            ));
            assert_eq!(
                result.report.status,
                Status::Invalid,
                "{}",
                result.render_human()
            );
        }
        for literal in ["(value: A())", "(value: (items: [A()]))"] {
            let result = check_source(&format!(
                "artifact A {{}}\nprocess Main {{ () => {{ let tuple = {literal} }} }}"
            ));
            assert_eq!(
                result.report.status,
                Status::Invalid,
                "{}",
                result.render_human()
            );
            assert!(result.report.diagnostics.iter().any(|d| d.code == "E0860"));
        }
    }

    #[test]
    fn named_tuple_wrong_field_types_and_refinements_are_checked() {
        for assignment in ["(width: 2 Second)", "(wrong: 2 Meter)"] {
            let result = check_source(&format!(
                "process Main {{ () => {{ let settings: (width: Length) = {assignment} }} }}"
            ));
            assert_eq!(
                result.report.status,
                Status::Invalid,
                "{}",
                result.render_human()
            );
        }
        let result = check_source(
            r#"
unit Positive = Scalar(x) { requires x > 0 }
process Main {
    () => {
        let settings: (nested: (scale: Positive)) = (nested: (scale: -1))
    }
}
"#,
        );
        assert_eq!(
            result.report.status,
            Status::Invalid,
            "{}",
            result.render_human()
        );
    }

    #[test]
    fn named_tuple_function_results_stay_compatible() {
        let result = check_source(
            r#"
fn summarize(value: (width: Length, valid: Bool)) -> (width: Length, valid: Bool) {
    width = value.width
    valid = value.valid
}
process Main {
    () => {
        let summary: (valid: Bool, width: Length) = summarize(value: (width: 2 Meter, valid: true))
        requires summary.width == 2 Meter
    }
}
"#,
        );
        assert_eq!(
            result.report.status,
            Status::Complete,
            "{}",
            result.render_human()
        );
        assert!(
            result.report.constraints.is_empty(),
            "{}",
            result.render_human()
        );
    }

    #[test]
    fn named_tuple_generic_fields_require_copyable_bounds() {
        for bound in ["where T: Copyable", ""] {
            let result = check_source(&format!(
                r#"
fn wrap[T](value: (item: T)) -> (result: (item: T)) {bound} {{
    let copy: (item: T) = value
    result = copy
}}
process Main {{ () => {{
    let result = wrap(value: (item: 2 Meter))
    requires result.result.item == 2 Meter
}} }}
"#
            ));
            assert_eq!(
                result.report.status,
                if bound.is_empty() {
                    Status::Invalid
                } else {
                    Status::Complete
                },
                "{}",
                result.render_human()
            );
            if !bound.is_empty() {
                assert!(
                    result.report.constraints.is_empty(),
                    "{}",
                    result.render_human()
                );
            }
        }
    }

    #[test]
    fn named_tuple_unknowns_remain_constraints_and_missing_values_obligations() {
        let result = check_source(
            r#"
unit Positive = Scalar(x) { requires x > 0 }
process Spec {
    config settings: (nested: (scale: Positive))
    () => {
        let copy = settings
        requires copy.nested.scale < 10
    }
}
process Main { () => { let run = Spec()() } }
"#,
        );
        assert_eq!(
            result.report.status,
            Status::Partial,
            "{}",
            result.render_human()
        );
        assert!(
            result.report.diagnostics.is_empty(),
            "{}",
            result.render_human()
        );
        assert!(!result.report.obligations.is_empty());
        assert_eq!(
            result.report.constraints.len(),
            2,
            "{}",
            result.render_human()
        );
    }

    #[test]
    fn named_tuple_measurements_do_not_move_artifacts_and_arrays_are_copyable() {
        let result = check_source(
            r#"
artifact Sample { width: Length }
process Main {
    gets sample: Sample
    export kept: Sample
    export measurements: (width: Length, samples: (width: Length)[])
    () => {
        let measured = (width: sample.width)
        let copies: (width: Length)[2] = [measured, measured]
        for entry in copies { requires entry.width > 0 Meter }
        measurements = (width: measured.width, samples: copies)
        kept = sample
    }
}
"#,
        );
        assert!(
            result.report.diagnostics.is_empty(),
            "{}",
            result.render_human()
        );
        assert!(
            !result.report.constraints.is_empty(),
            "{}",
            result.render_human()
        );
        for bad_type in [
            "(a: Stream[Sample])",
            "(a: Sample[])",
            "(a: (b: Missing[]))",
            "(a: Configured[Main])",
        ] {
            let bad = check_source(&format!(
                "artifact Sample {{}}\nprocess Main {{ export measurements: {bad_type} }}"
            ));
            assert_eq!(bad.report.status, Status::Invalid, "{}", bad.render_human());
        }
    }

    #[test]
    fn named_tuple_trait_coherence_ignores_field_order_and_unit_spelling() {
        let result = check_source(
            r#"
trait Check[T] { fn check(value: T) -> (valid: Bool) }
impl Check[(width: Length, valid: Bool)] {
    fn check(value: (width: Length, valid: Bool)) -> (valid: Bool) { valid = true }
}
impl Check[(valid: Bool, width: Meter)] {
    fn check(value: (valid: Bool, width: Meter)) -> (valid: Bool) { valid = true }
}
"#,
        );
        assert_eq!(
            result.report.status,
            Status::Invalid,
            "{}",
            result.render_human()
        );
        assert!(
            result
                .report
                .diagnostics
                .iter()
                .any(|diagnostic| diagnostic.code == "E0404"),
            "{}",
            result.render_human()
        );
    }

    #[test]
    fn metric_dependencies_are_unresolved_value_paths_not_helper_names() {
        let current = Expr::name("current", 0..7);
        let package = Expr {
            kind: ExprKind::Member {
                value: Box::new(current),
                member: "package".into(),
            },
            span: 0..14,
        };
        let length = Expr {
            kind: ExprKind::Member {
                value: Box::new(package),
                member: "length".into(),
            },
            span: 0..21,
        };
        let expression = Expr {
            kind: ExprKind::Call {
                callee: Box::new(Expr::name("package_processing_plan", 22..45)),
                arguments: vec![
                    CallArgument {
                        name: Some("package_length".into()),
                        value: length,
                        span: 46..67,
                    },
                    CallArgument {
                        name: Some("job_count".into()),
                        value: Expr {
                            kind: ExprKind::Number {
                                value: "1000".into(),
                                suffix: None,
                            },
                            span: 68..72,
                        },
                        span: 68..72,
                    },
                ],
            },
            span: 0..73,
        };

        assert_eq!(
            expression_value_dependencies(&expression),
            ["current.package.length"]
        );
    }

    #[test]
    fn schedules_repeated_jobs_against_divisible_resource_capacity() {
        let compilation = check_source(
            r#"
                import math
                resource Pool { capacity: 2 }

                process Stage {
                    uses pool: Pool[1]
                    export duration: Time aggregate makespan
                    () => { duration = 3 Second }
                }

                process Factory {
                    uses pools: Pool[2]
                    export duration: Time aggregate makespan
                    monitor duration
                    () => {
                        for job in math.range(0, 3) {
                            repeat 2 {
                                let stage = Stage()()
                                pools -> stage.pool
                            }
                        }
                    }
                }
            "#,
        );
        assert_eq!(
            compilation.report.status,
            Completeness::Complete,
            "{}",
            compilation.render_human()
        );
        let schedule = compilation
            .report
            .resource_schedules
            .iter()
            .find(|schedule| schedule.path == "Factory.schedule[0]")
            .expect("resource schedule");
        assert_eq!(schedule.jobs, 3);
        assert_eq!(schedule.repetitions, 2);
        assert_eq!(schedule.operation_count, 6);
        assert_eq!(schedule.makespan_seconds, "9");
        assert_eq!(schedule.resources[0].parallel_capacity, 2);
        assert!(
            schedule.stages[0]
                .activity
                .iter()
                .any(|bucket| bucket.average_active_operations != "0")
        );
        assert!(
            schedule.stages[0]
                .activity
                .iter()
                .any(|bucket| bucket.average_queued_operations != "0")
        );
    }

    #[test]
    fn stream_consumers_overlap_after_first_item_and_wait_for_close() {
        let compilation = check_source(
            r#"
                import math
                artifact Token {}
                resource Slot { capacity: 1 }

                process Produce {
                    export tokens: Stream[Token]
                    export duration: Time aggregate makespan
                    () => {
                        yield tokens = Token() after 2 Second
                        duration = 10 Second
                    }
                }

                process Consume {
                    gets tokens: Stream[Token]
                    uses slot: Slot[1]
                    export duration: Time aggregate makespan
                    () => {
                        for token in tokens {}
                        duration = 3 Second
                    }
                }

                process Factory {
                    uses slots: Slot[1]
                    () => {
                        for job in math.range(0, 1) {
                            let produce = Produce()()
                            let consume = Consume()()
                            produce.tokens -> consume.tokens
                            slots -> consume.slot
                        }
                    }
                }
            "#,
        );
        assert_eq!(
            compilation.report.status,
            Completeness::Complete,
            "{}",
            compilation.render_human()
        );
        let schedule = compilation
            .report
            .resource_schedules
            .iter()
            .find(|schedule| schedule.path == "Factory.schedule[0]")
            .expect("stream schedule");
        assert_eq!(schedule.makespan_seconds, "10");
        let consumer = schedule
            .stages
            .iter()
            .find(|stage| stage.path == "Factory.consume")
            .expect("consumer stage");
        assert_eq!(consumer.earliest_ready_seconds, "2");
        assert_eq!(consumer.earliest_start_seconds, "2");
        assert_eq!(consumer.latest_finish_seconds, "10");
        assert!(
            consumer
                .activity
                .iter()
                .any(|bucket| bucket.average_active_operations != "0")
        );
        assert!(
            consumer
                .activity
                .iter()
                .any(|bucket| bucket.average_waiting_operations != "0")
        );
        assert_eq!(schedule.resources[0].utilization, "0.8");
    }

    #[test]
    fn empty_streams_wait_for_close_and_zero_latency_transforms_chain() {
        let empty = check_source(
            r#"
                import math
                artifact Token {}
                resource Slot { capacity: 1 }

                process ProduceEmpty {
                    export tokens: Stream[Token]
                    export duration: Time aggregate makespan
                    () => {
                        requires cardinality(tokens) == 0
                        for index in math.range(0, 0) {
                            yield tokens = Token() after 1 Second
                        }
                        duration = 5 Second
                    }
                }
                process Consume {
                    gets tokens: Stream[Token]
                    uses slot: Slot[1]
                    export duration: Time aggregate makespan
                    () => {
                        for token in tokens {}
                        duration = 2 Second
                    }
                }
                process EmptyPipeline {
                    uses slot: Slot[1]
                    () => {
                        for job in math.range(0, 1) {
                            let producer = ProduceEmpty()()
                            let consumer = Consume()()
                            producer.tokens -> consumer.tokens
                            slot -> consumer.slot
                        }
                    }
                }
            "#,
        );
        assert_eq!(
            empty.report.status,
            Completeness::Complete,
            "{}",
            empty.render_human()
        );
        let schedule = empty
            .report
            .resource_schedules
            .first()
            .unwrap_or_else(|| panic!("missing empty schedule\n{}", empty.render_human()));
        let consumer = schedule
            .stages
            .iter()
            .find(|stage| stage.path.ends_with(".consumer"))
            .expect("empty-stream consumer");
        assert_eq!(consumer.earliest_start_seconds, "5");
        assert_eq!(consumer.latest_finish_seconds, "7");

        let chained = check_source(
            r#"
                import math
                artifact Token {}
                resource Slot { capacity: 1 }

                process Produce {
                    export tokens: Stream[Token]
                    export duration: Time aggregate makespan
                    () => {
                        yield tokens = Token() after 2 Second
                        duration = 10 Second
                    }
                }
                process Transform {
                    gets input: Stream[Token]
                    export output: Stream[Token]
                    export duration: Time aggregate makespan
                    () => {
                        for token in input {
                            yield output = token after 0 Second
                        }
                        duration = 1 Second
                    }
                }
                process Consume {
                    gets input: Stream[Token]
                    uses slot: Slot[1]
                    export duration: Time aggregate makespan
                    () => {
                        for token in input {}
                        duration = 1 Second
                    }
                }
                process ChainedPipeline {
                    uses slot: Slot[1]
                    () => {
                        for job in math.range(0, 1) {
                            let producer = Produce()()
                            let transform = Transform()()
                            let consumer = Consume()()
                            producer.tokens -> transform.input
                            transform.output -> consumer.input
                            slot -> consumer.slot
                        }
                    }
                }
            "#,
        );
        assert_eq!(
            chained.report.status,
            Completeness::Complete,
            "{}",
            chained.render_human()
        );
        let schedule = &chained.report.resource_schedules[0];
        for suffix in [".transform", ".consumer"] {
            let stage = schedule
                .stages
                .iter()
                .find(|stage| stage.path.ends_with(suffix))
                .unwrap_or_else(|| panic!("missing {suffix}"));
            assert_eq!(stage.earliest_start_seconds, "2");
            assert_eq!(stage.latest_finish_seconds, "10");
        }
        assert_eq!(schedule.makespan_seconds, "10");
    }

    #[test]
    fn a_consumer_waits_for_every_input_stream_close() {
        let compilation = check_source(
            r#"
                import math
                artifact Token {}
                resource Slot { capacity: 1 }

                process First {
                    export tokens: Stream[Token]
                    export duration: Time aggregate makespan
                    () => {
                        yield tokens = Token() after 2 Second
                        duration = 10 Second
                    }
                }
                process Second {
                    export tokens: Stream[Token]
                    export duration: Time aggregate makespan
                    () => {
                        yield tokens = Token() after 4 Second
                        duration = 6 Second
                    }
                }
                process Join {
                    gets left: Stream[Token]
                    gets right: Stream[Token]
                    uses slot: Slot[1]
                    export duration: Time aggregate makespan
                    () => {
                        for token in left {}
                        for token in right {}
                        duration = 3 Second
                    }
                }
                process Pipeline {
                    uses slot: Slot[1]
                    () => {
                        for job in math.range(0, 1) {
                            let first = First()()
                            let second = Second()()
                            let join = Join()()
                            first.tokens -> join.left
                            second.tokens -> join.right
                            slot -> join.slot
                        }
                    }
                }
            "#,
        );
        assert_eq!(
            compilation.report.status,
            Completeness::Complete,
            "{}",
            compilation.render_human()
        );
        let join = compilation.report.resource_schedules[0]
            .stages
            .iter()
            .find(|stage| stage.path.ends_with(".join"))
            .expect("join stage");
        assert_eq!(join.earliest_start_seconds, "4");
        assert_eq!(join.latest_finish_seconds, "10");
    }

    #[test]
    fn physical_repeats_drive_ownership_metrics_and_resource_scheduling() {
        let compilation = check_source(
            r#"
                import std.process (CostAccounting, DurationAccounting)

                artifact Item {}
                artifact Intermediate { item: Item }
                resource Pool { capacity: 2 }
                resource Slot { capacity: 1 }

                process StepA: CostAccounting, DurationAccounting {
                    gets item: Item
                    uses pool: Pool[1]
                    export intermediate: Intermediate
                    () => {
                        intermediate = Intermediate(item: item)
                        cost = 2 USD
                        duration = 1 Second
                    }
                }

                process StepB: CostAccounting, DurationAccounting {
                    gets intermediate: Intermediate
                    uses slot: Slot[1]
                    export returned: Item
                    () => {
                        returned = intermediate.item
                        cost = 3 USD
                        duration = 2 Second
                    }
                }

                process Factory: CostAccounting, DurationAccounting {
                    gets items: Stream[Item]
                    uses pools: Pool[2]
                    uses slots: Slot[1]
                    export completed: Stream[Item]
                    expose config item_count: Integer = 3
                    expose config round_count: Integer = 2
                    monitor cost

                    () => {
                        requires cardinality(items) == item_count
                        for item in items {
                            let mut current = item
                            repeat round_count {
                                let step_a = StepA()()
                                current -> step_a.item
                                pools -> step_a.pool

                                let step_b = StepB()()
                                step_a.intermediate -> step_b.intermediate
                                slots -> step_b.slot
                                current = step_b.returned
                            }
                            yield completed = current
                        }
                    }
                }
            "#,
        );
        assert_eq!(
            compilation.report.status,
            Completeness::Complete,
            "{}",
            compilation.render_human()
        );
        let cost = compilation
            .report
            .monitored_metrics
            .iter()
            .find(|metric| metric.path == "Factory.cost")
            .expect("factory cost");
        assert_eq!(cost.value, "30 USD");
        let schedule = compilation
            .report
            .resource_schedules
            .iter()
            .find(|schedule| schedule.path == "Factory.schedule[0]")
            .expect("physical resource schedule");
        assert_eq!(schedule.jobs, 3);
        assert_eq!(schedule.repetitions, 2);
        assert_eq!(schedule.operation_count, 12);
        assert_eq!(schedule.makespan_seconds, "13");
        assert_eq!(schedule.stages[0].operation_count, 6);
        assert_eq!(schedule.stages[1].operation_count, 6);
        let step_a = schedule
            .stages
            .iter()
            .find(|stage| stage.path == "Factory.step_a")
            .expect("step_a stage");
        let step_b = schedule
            .stages
            .iter()
            .find(|stage| stage.path == "Factory.step_b")
            .expect("step_b stage");
        assert_eq!(step_a.earliest_start_seconds, "0");
        assert_eq!(step_a.latest_finish_seconds, "8");
        assert_eq!(step_b.earliest_start_seconds, "1");
        assert_eq!(step_b.latest_finish_seconds, "13");
        let wires = &compilation.model.processes["Factory"].wires;
        assert!(wires.iter().all(|wire| wire.source != "current"));
        assert!(
            wires
                .iter()
                .any(|wire| wire.source == "items" && wire.target == "step_a.item")
        );
    }

    #[test]
    fn physical_repeats_pipeline_independent_tails_through_loop_carried_values() {
        let compilation = check_source(
            r#"
                import math

                artifact Item {}
                artifact Intermediate { item: Item }
                artifact Data {}

                resource Pool { capacity: 1 }
                resource Slot { capacity: 1 }
                resource CPU { capacity: 1 }
                resource Depot { capacity: 1 }

                process StepA {
                    gets item: Item
                    uses pool: Pool[1]
                    export intermediate: Intermediate
                    export duration: Time aggregate makespan
                    () => {
                        intermediate = Intermediate(item: item)
                        duration = 1 Second
                    }
                }

                process StepB {
                    gets intermediate: Intermediate
                    uses slot: Slot[1]
                    export returned: Item
                    export data: Data
                    export duration: Time aggregate makespan
                    () => {
                        returned = intermediate.item
                        data = Data()
                        duration = 2 Second
                    }
                }

                process StepC {
                    gets data: Data
                    uses cpu: CPU[1]
                    export duration: Time aggregate makespan
                    () => {
                        duration = 10 Second
                    }
                }

                process StepD {
                    gets item: Item
                    uses depot: Depot[1]
                    export duration: Time aggregate makespan
                    () => {
                        duration = 1 Second
                    }
                }

                process Pipeline {
                    uses pools: Pool[1]
                    uses slots: Slot[1]
                    uses cpus: CPU[1]
                    uses depots: Depot[1]

                    () => {
                        for job in math.range(0, 1) {
                            let mut current = Item()
                            repeat 2 {
                                let step_a = StepA()()
                                current -> step_a.item
                                pools -> step_a.pool

                                let step_b = StepB()()
                                step_a.intermediate -> step_b.intermediate
                                slots -> step_b.slot

                                let step_c = StepC()()
                                step_b.data -> step_c.data
                                cpus -> step_c.cpu

                                current = step_b.returned
                            }

                            let step_d = StepD()()
                            current -> step_d.item
                            depots -> step_d.depot
                        }
                    }
                }
            "#,
        );
        assert_eq!(
            compilation.report.status,
            Completeness::Complete,
            "{}",
            compilation.render_human()
        );
        let schedule = compilation
            .report
            .resource_schedules
            .iter()
            .find(|schedule| schedule.path == "Pipeline.schedule[0]")
            .expect("pipelined repeat schedule");
        assert_eq!(schedule.repetitions, 2);
        assert_eq!(schedule.operation_count, 7);
        assert_eq!(schedule.makespan_seconds, "23");

        let stage = |suffix: &str| {
            schedule
                .stages
                .iter()
                .find(|stage| stage.path.ends_with(suffix))
                .unwrap_or_else(|| panic!("missing stage {suffix}"))
        };
        assert_eq!(stage(".step_a").earliest_start_seconds, "0");
        assert_eq!(stage(".step_a").latest_finish_seconds, "4");
        assert_eq!(stage(".step_b").earliest_start_seconds, "1");
        assert_eq!(stage(".step_b").latest_finish_seconds, "6");
        assert_eq!(stage(".step_c").earliest_start_seconds, "3");
        assert_eq!(stage(".step_c").latest_finish_seconds, "23");
        assert_eq!(stage(".step_d").earliest_start_seconds, "6");
        assert_eq!(stage(".step_d").latest_finish_seconds, "7");
    }

    #[test]
    fn repeat_requires_owned_loop_carried_state_to_be_restored() {
        let compilation = check_source(
            r#"
                artifact Item {}
                process Consume { gets item: Item () => {} }
                process Invalid {
                    gets item: Item
                    () => {
                        let mut current = item
                        repeat 2 {
                            let consume = Consume()()
                            current -> consume.item
                        }
                    }
                }
            "#,
        );
        assert_eq!(compilation.report.status, Completeness::Invalid);
        assert!(
            compilation
                .report
                .diagnostics
                .iter()
                .any(|diagnostic| diagnostic.code == "E0647"),
            "{}",
            compilation.render_human()
        );
    }

    #[test]
    fn physical_schedule_parallelizes_incomparable_operations() {
        let compilation = check_source(
            r#"
                import math
                resource LeftPool { capacity: 1 }
                resource RightPool { capacity: 1 }

                process Left {
                    uses slot: LeftPool[1]
                    export duration: Time aggregate makespan
                    () => { duration = 3 Second }
                }
                process Right {
                    uses slot: RightPool[1]
                    export duration: Time aggregate makespan
                    () => { duration = 5 Second }
                }
                process Parallel {
                    uses left_pool: LeftPool[1]
                    uses right_pool: RightPool[1]
                    export duration: Time aggregate makespan
                    () => {
                        for job in math.range(0, 1) {
                            let left = Left()()
                            left_pool -> left.slot
                            let right = Right()()
                            right_pool -> right.slot
                        }
                    }
                }
            "#,
        );
        assert_eq!(
            compilation.report.status,
            Completeness::Complete,
            "{}",
            compilation.render_human()
        );
        let schedule = &compilation.report.resource_schedules[0];
        assert_eq!(schedule.operation_count, 2);
        assert_eq!(schedule.makespan_seconds, "5");
        assert!(
            schedule
                .stages
                .iter()
                .all(|stage| stage.earliest_start_seconds == "0")
        );
    }

    #[test]
    fn repeat_zero_is_empty_and_negative_counts_are_rejected() {
        let empty = check_source(
            r#"
                process Empty {
                    () => {
                        repeat 0 {
                            requires false
                        }
                    }
                }
            "#,
        );
        assert_eq!(
            empty.report.status,
            Completeness::Complete,
            "{}",
            empty.render_human()
        );

        let negative = check_source(
            r#"
                process Negative {
                    () => {
                        repeat -1 {}
                    }
                }
            "#,
        );
        assert_eq!(negative.report.status, Completeness::Invalid);
        assert!(
            negative
                .report
                .diagnostics
                .iter()
                .any(|diagnostic| diagnostic.code == "E0641")
        );
    }

    fn check_source_with_overrides(source: &str, overrides: Vec<ConfigOverride>) -> Compilation {
        let directory = tempdir().unwrap();
        let path = directory.path().join("main.req");
        fs::write(&path, source).unwrap();
        Compiler::new(CompilerOptions {
            overrides,
            ..CompilerOptions::default()
        })
        .check_path(&path)
        .unwrap()
    }

    #[test]
    fn exposed_resource_capacity_changes_the_exact_schedule() {
        let source = r#"
            import math
            resource Slot { capacity: 4 }

            process Stage {
                uses slot: Slot[1]
                export duration: Time aggregate makespan
                () => { duration = 3 Second }
            }

            process Factory {
                expose config lane_count: Integer = 2
                uses slots: Slot[lane_count]
                export duration: Time aggregate makespan
                () => {
                    for job in math.range(0, 4) {
                        repeat 2 {
                            let stage = Stage()()
                            slots -> stage.slot
                        }
                    }
                }
            }
        "#;
        let default = check_source(source);
        let default_schedule = &default.report.resource_schedules[0];
        assert_eq!(default_schedule.makespan_seconds, "12");
        assert_eq!(default_schedule.resources[0].parallel_capacity, 2);

        let overridden = check_source_with_overrides(
            source,
            vec![ConfigOverride {
                path: "Factory.lane_count".into(),
                value: "4".into(),
            }],
        );
        let overridden_schedule = &overridden.report.resource_schedules[0];
        assert_eq!(overridden_schedule.makespan_seconds, "6");
        assert_eq!(overridden_schedule.resources[0].parallel_capacity, 4);
    }

    #[test]
    fn capacity_resources_are_limited_to_physical_ports() {
        let compilation = check_source(
            r#"
                resource Slot { capacity: 1 }

                artifact InvalidArtifact { slot: Slot }

                fn invalid_value(slot: Slot) -> (ok: Bool) {
                    ok = true
                }

                process InvalidProcess {
                    config slot: Slot
                }

                process Overclaim {
                    uses slots: Slot[2]
                }
            "#,
        );
        let messages = compilation
            .report
            .diagnostics
            .iter()
            .map(|diagnostic| diagnostic.message.as_str())
            .collect::<Vec<_>>();
        assert!(
            messages
                .iter()
                .any(|message| message.contains("cannot be stored"))
        );
        assert!(
            messages
                .iter()
                .any(|message| message.contains("not pure-function values"))
        );
        assert!(
            messages
                .iter()
                .any(|message| message.contains("uses ports or provisioned through exports"))
        );
        assert!(
            messages
                .iter()
                .any(|message| message.contains("exceeds Slot capacity"))
        );
    }

    #[test]
    fn a_process_can_provision_and_fan_out_one_resource_pool() {
        let compilation = check_source(
            r#"
                resource Slot { capacity: 4 }

                process Provision {
                    config lane_count: Integer
                    export slots: Slot[lane_count]
                    () => { slots = Slot[lane_count] }
                }

                process Work {
                    uses slot: Slot[claim]
                    (claim: Integer) => {}
                }

                process Factory {
                    () => {
                        let provision = Provision(lane_count: 2)()
                        let first = Work()(claim: 1)
                        let second = Work()(claim: 1)
                        provision.slots -> first.slot
                        provision.slots -> second.slot
                    }
                }
            "#,
        );
        assert_eq!(
            compilation.report.status,
            Completeness::Complete,
            "{}",
            compilation.render_human()
        );
        let wires = &compilation.model.processes["Factory"].wires;
        assert_eq!(
            wires
                .iter()
                .filter(|wire| wire.source == "provision.slots" && wire.ownership == "loan")
                .count(),
            2
        );
        assert_eq!(compilation.report.provisioned_resources.len(), 1);
        let pool = &compilation.report.provisioned_resources[0];
        assert_eq!(pool.path, "Factory.provision.slots");
        assert_eq!(pool.resource, "Slot");
        assert_eq!(pool.capacity, "2");
        assert_eq!(
            pool.borrowers,
            ["Factory.first.slot", "Factory.second.slot"]
        );
        assert!(
            !compilation
                .report
                .constraints
                .iter()
                .any(|constraint| constraint.predicate.contains("non_overlap"))
        );
    }

    #[test]
    fn direct_sibling_claims_share_one_provisioned_pool_in_time() {
        let source = r#"
            resource Slot { capacity: 2 }

            process Provision {
                export slots: Slot[2]
                export duration: Time aggregate makespan
                () => {
                    slots = Slot[2]
                    duration = 5 Second
                }
            }

            process Work {
                uses slots: Slot[claim]
                export duration: Time aggregate makespan
                (claim: Integer) => { duration = 3 Second }
            }

            process Factory {
                export duration: Time aggregate makespan
                () => {
                    let provision = Provision()()
                    let first = Work()(claim: 2)
                    let second = Work()(claim: 2)
                    provision.slots -> first.slots
                    provision.slots -> second.slots
                }
            }
        "#;
        let compilation = check_source(source);
        assert_eq!(
            compilation.report.status,
            Completeness::Complete,
            "{}",
            compilation.render_human()
        );
        let schedule = compilation
            .report
            .resource_schedules
            .iter()
            .find(|schedule| schedule.path == "Factory.schedule[0]")
            .expect("direct resource schedule");
        let first = schedule
            .stages
            .iter()
            .find(|stage| stage.path == "Factory.first")
            .unwrap();
        let second = schedule
            .stages
            .iter()
            .find(|stage| stage.path == "Factory.second")
            .unwrap();
        assert_eq!(first.earliest_start_seconds, "5");
        assert_eq!(first.latest_finish_seconds, "8");
        assert_eq!(second.earliest_start_seconds, "8");
        assert_eq!(second.latest_finish_seconds, "11");
        assert_eq!(
            compilation.report.provisioned_resources[0]
                .available_after_seconds
                .as_deref(),
            Some("5")
        );
        assert_eq!(
            compilation.report.provisioned_resources[0]
                .utilization
                .as_deref(),
            Some("1")
        );
    }

    #[test]
    fn a_pool_is_not_available_before_its_producer_can_complete() {
        let source = r#"
            import std.process (DurationAccounting)
            resource Slot { capacity: 1 }

            process Provision: DurationAccounting {
                export slots: Slot[1]
                () => { slots = Slot[1] }
            }
            process Work: DurationAccounting {
                uses slot: Slot[1]
                () => { duration = 1 Second }
            }
            process Factory: DurationAccounting {
                () => {
                    let provision = Provision()()
                    let work = Work()()
                    provision.slots -> work.slot
                }
            }
        "#;
        let compilation = check_source(source);
        assert_eq!(compilation.report.status, Completeness::Partial);
        assert!(
            compilation
                .report
                .obligations
                .iter()
                .any(|obligation| { obligation.path == "Factory.provision.duration" })
        );
        assert_eq!(
            compilation.report.provisioned_resources[0].available_after_seconds,
            None
        );
        assert!(compilation.report.resource_schedules.is_empty());
    }

    #[test]
    fn sibling_composite_schedules_do_not_clone_a_shared_pool() {
        let source = r#"
            import math
            resource Slot { capacity: 2 }

            process Provision {
                export slots: Slot[2]
                export duration: Time aggregate makespan
                () => {
                    slots = Slot[2]
                    duration = 5 Second
                }
            }
            process Stage {
                uses slots: Slot[2]
                export duration: Time aggregate makespan
                () => { duration = 3 Second }
            }
            process Batch {
                uses slots: Slot[2]
                export duration: Time aggregate makespan
                () => {
                    for job in math.range(0, 2) {
                        let stage = Stage()()
                        slots -> stage.slots
                    }
                }
            }
            process Factory {
                export duration: Time aggregate makespan
                () => {
                    let provision = Provision()()
                    let first = Batch()()
                    let second = Batch()()
                    provision.slots -> first.slots
                    provision.slots -> second.slots
                }
            }
        "#;
        let compilation = check_source(source);
        assert_eq!(
            compilation.report.status,
            Completeness::Complete,
            "{}",
            compilation.render_human()
        );
        let first = compilation
            .report
            .resource_schedules
            .iter()
            .find(|schedule| schedule.path == "Factory.first.schedule[0]")
            .unwrap();
        let second = compilation
            .report
            .resource_schedules
            .iter()
            .find(|schedule| schedule.path == "Factory.second.schedule[0]")
            .unwrap();
        assert_eq!(first.start_offset_seconds, "5");
        assert_eq!(first.makespan_seconds, "11");
        assert_eq!(second.start_offset_seconds, "5");
        assert_eq!(second.stages[0].earliest_start_seconds, "11");
        assert_eq!(second.makespan_seconds, "17");
        assert_eq!(
            compilation.report.provisioned_resources[0]
                .utilization
                .as_deref(),
            Some("1")
        );
    }

    #[test]
    fn physical_stream_jobs_keep_item_specific_durations() {
        let source = r#"
            resource Slot { capacity: 1 }
            artifact Job { work: Time }

            process Produce {
                export jobs: Stream[Job]
                export duration: Time aggregate makespan
                () => {
                    yield jobs = Job(work: 2 Second) after 0 Second
                    yield jobs = Job(work: 5 Second) after 0 Second
                    requires cardinality(jobs) == 2
                    duration = 0 Second
                }
            }
            process Work {
                gets job: Job
                uses slot: Slot[1]
                export duration: Time aggregate makespan
                () => { duration = job.work }
            }
            process Factory {
                uses slots: Slot[1]
                export duration: Time aggregate makespan
                () => {
                    let producer = Produce()()
                    for job in producer.jobs {
                        let work = Work()()
                        job -> work.job
                        slots -> work.slot
                    }
                }
            }
        "#;
        let compilation = check_source(source);
        assert_eq!(
            compilation.report.status,
            Completeness::Complete,
            "{}",
            compilation.render_human()
        );
        let schedule = compilation
            .report
            .resource_schedules
            .iter()
            .find(|schedule| schedule.path == "Factory.schedule[0]")
            .unwrap();
        assert_eq!(schedule.makespan_seconds, "7");
        assert_eq!(schedule.stages[0].duration_seconds, "3.5");
        assert_eq!(schedule.stages[0].minimum_duration_seconds, "2");
        assert_eq!(schedule.stages[0].maximum_duration_seconds, "5");
    }

    #[test]
    fn provisioned_resource_reexports_keep_one_origin_and_enforce_actual_capacity() {
        let valid = check_source(
            r#"
                resource Slot { capacity: 8 }

                process Provision {
                    export slots: Slot[2]
                    () => { slots = Slot[2] }
                }
                process Reexport {
                    export slots: Slot[2]
                    () => {
                        let provision = Provision()()
                        provision.slots -> slots
                    }
                }
                process Work {
                    uses slots: Slot[claim]
                    (claim: Integer) => {}
                }
                process Factory {
                    () => {
                        let reexport = Reexport()()
                        let work = Work()(claim: 2)
                        reexport.slots -> work.slots
                    }
                }
            "#,
        );
        assert_eq!(
            valid.report.status,
            Completeness::Complete,
            "{}",
            valid.render_human()
        );
        let pool = valid
            .report
            .provisioned_resources
            .first()
            .expect("one original pool");
        assert_eq!(pool.path, "Factory.reexport.provision.slots");
        assert_eq!(pool.aliases, ["Factory.reexport.slots"]);
        assert_eq!(pool.borrowers, ["Factory.work.slots"]);

        let invalid = check_source(
            r#"
                resource Slot { capacity: 8 }
                process Provision {
                    export slots: Slot[2]
                    () => { slots = Slot[2] }
                }
                process Work {
                    uses slots: Slot[claim]
                    (claim: Integer) => {}
                }
                process Factory {
                    () => {
                        let provision = Provision()()
                        let work = Work()(claim: 3)
                        provision.slots -> work.slots
                    }
                }
            "#,
        );
        assert_eq!(invalid.report.status, Completeness::Invalid);
        assert!(invalid.report.diagnostics.iter().any(|diagnostic| {
            diagnostic.code == "E0727"
                && diagnostic.message.contains("Factory.work.slots")
                && diagnostic.message.contains("capacity 2")
        }));
    }

    #[test]
    fn bodyless_resource_exports_are_open_provisioning_obligations() {
        let compilation = check_source(
            r#"
                resource Slot { capacity: 4 }
                process SetupPool {
                    export slots: Slot[2]
                }
            "#,
        );
        assert_eq!(compilation.report.status, Completeness::Partial);
        assert!(compilation.report.obligations.iter().any(|obligation| {
            obligation.path == "SetupPool.slots" && obligation.kind == "missing_export"
        }));
        assert!(compilation.report.provisioned_resources.is_empty());
    }

    #[test]
    fn resource_constructors_are_not_general_values() {
        let compilation = check_source(
            r#"
                resource Slot { capacity: 4 }
                process Invalid {
                    () => { let slots = Slot[2] }
                }
            "#,
        );
        assert!(compilation.report.diagnostics.iter().any(|diagnostic| {
            diagnostic
                .message
                .contains("resource pools cannot be bound to locals")
        }));
    }

    #[test]
    fn overrides_are_typed_and_limited_to_exposed_configuration() {
        let source = r#"
process Root {
    expose config visible: Integer = 2
    config hidden: Integer = 3
}
"#;
        let overridden = check_source_with_overrides(
            source,
            vec![ConfigOverride {
                path: "Root.visible".into(),
                value: "7".into(),
            }],
        );
        let root = &overridden.model.processes["Root"];
        assert_eq!(root.default_config["visible"], "7");
        assert_eq!(
            root.default_config_sources["visible"],
            ConfigValueSource::Overridden
        );
        assert_eq!(overridden.overrides["Root.visible"], "7");

        let hidden = check_source_with_overrides(
            source,
            vec![ConfigOverride {
                path: "Root.hidden".into(),
                value: "7".into(),
            }],
        );
        let diagnostic = hidden
            .report
            .diagnostics
            .iter()
            .find(|diagnostic| diagnostic.code == "E0523")
            .expect("unexposed override diagnostic");
        assert!(diagnostic.message.contains("Root.hidden was not exposed"));
        assert!(
            diagnostic
                .message
                .contains("try \"expose config hidden: Integer\" in main.req:4")
        );

        let wrong_type = check_source_with_overrides(
            source,
            vec![ConfigOverride {
                path: "Root.visible".into(),
                value: "true".into(),
            }],
        );
        assert!(
            wrong_type
                .report
                .diagnostics
                .iter()
                .any(|diagnostic| diagnostic.code == "E0526"
                    && diagnostic.message.contains("expects Integer, found Bool"))
        );
    }

    #[test]
    fn valid_partial_process_reports_missing_export() {
        let directory = tempdir().unwrap();
        let path = directory.path().join("main.req");
        fs::write(
            &path,
            "process P: DurationAccounting { () => { requires true } }\n",
        )
        .unwrap();
        fs::write(directory.path().join("unused.req"), "artifact Unused {}\n").unwrap();
        // Use a local contract because this fixture does not import std.
        fs::write(
            &path,
            "process DurationAccounting { export duration: Time aggregate makespan }\nprocess P: DurationAccounting { () => { requires true } }\n",
        )
        .unwrap();
        let compilation = Compiler::new(CompilerOptions::default())
            .check_path(&path)
            .unwrap();
        assert_eq!(compilation.report.status, Completeness::Partial);
        assert!(
            compilation
                .report
                .obligations
                .iter()
                .any(|obligation| obligation.path == "P.duration")
        );
    }

    #[test]
    fn configuration_defaults_resolve_dependencies_and_process_arguments_pun() {
        let compilation = check_source(
            r#"
                process Child {
                    config scale: Integer = 9
                    (count: Integer) => {
                        requires count == scale
                    }
                }
                process Root {
                    config count: Integer = 4
                    config scale: Integer = count
                    () => {
                        let child = Child(scale)(count)
                    }
                }
            "#,
        );
        assert_eq!(
            compilation.report.status,
            Completeness::Complete,
            "{:?}",
            compilation.report.diagnostics
        );
        assert!(compilation.report.constraints.is_empty());
        let root = &compilation.model.processes["Root"];
        assert_eq!(root.default_config["count"], "4");
        assert_eq!(root.default_config["scale"], "4");
        assert_eq!(
            root.default_config_sources["scale"],
            ConfigValueSource::Defaulted
        );
        let child = &root.runs["child"];
        assert_eq!(child.config["scale"], "scale");
        assert_eq!(child.config_sources["scale"], ConfigValueSource::Supplied);
        assert_eq!(child.arguments["count"], "count");
    }

    #[test]
    fn exposed_configuration_is_explicit_and_coalesces_monotonically() {
        let compilation = check_source(
            r#"
                process Base {
                    expose config amount: Integer = 2
                }
                process Derived: Base {}
                process Internal {
                    config amount: Integer = 2
                }
                process Combined: Base, Internal {}
            "#,
        );

        assert!(compilation.model.processes["Base"].members["amount"].exposed);
        assert!(
            compilation.model.processes["Derived"].members["amount"].exposed,
            "inherited configuration must retain its exposure contract"
        );
        assert!(!compilation.model.processes["Internal"].members["amount"].exposed);
        assert!(compilation.model.processes["Combined"].members["amount"].exposed);
        assert!(
            !compilation
                .report
                .diagnostics
                .iter()
                .any(|diagnostic| diagnostic.code == "E0503"),
            "an unexposed parent does not revoke another parent's exposure promise"
        );
    }

    #[test]
    fn configuration_defaults_reject_cycles_and_invocation_dependencies() {
        let compilation = check_source(
            r#"
                process Bad {
                    config left: Integer = right
                    config right: Integer = left
                    (input: Integer) => {}
                }
                process AlsoBad {
                    config value: Integer = input
                    (input: Integer) => {}
                }
            "#,
        );
        assert!(
            compilation
                .report
                .diagnostics
                .iter()
                .any(|diagnostic| diagnostic.code == "E0517")
        );
        assert!(compilation.report.diagnostics.iter().any(|diagnostic| {
            diagnostic.code == "E0515" && diagnostic.message.contains("invocation parameter")
        }));
    }

    #[test]
    fn ordinary_pure_function_calls_remain_positional() {
        let compilation = check_source(
            r#"
                fn identity(value: Integer) -> (result: Integer) {
                    result = value
                }
                process P {
                    () => {
                        let value = 3
                        let result = identity(value)
                        requires result.result == 3
                    }
                }
            "#,
        );
        assert_eq!(
            compilation.report.status,
            Completeness::Complete,
            "{:?}",
            compilation.report.diagnostics
        );
    }

    #[test]
    fn generic_aggregate_exports_roll_up_composites() {
        let compilation = check_source(
            r#"
                process MassAccounting {
                    export material: Mass aggregate sum
                }
                process Leaf: MassAccounting {
                    () => { material = 2 Kilogram }
                }
                process Factory: MassAccounting {
                    monitor material
                    () => {
                        let leaf_process = Leaf()
                        let leaf = leaf_process()
                    }
                }
            "#,
        );
        assert_eq!(
            compilation.report.status,
            Completeness::Complete,
            "{:?}",
            compilation.report.diagnostics
        );
        assert!(compilation.report.assumed_zero.is_empty());
        assert!(compilation.report.constraints.iter().any(|constraint| {
            constraint.path == "Factory.completion.aggregate[material]"
                && constraint.predicate == "material == leaf.material"
                && constraint.phase == "completion"
        }));
        assert_eq!(
            compilation.model.processes["Factory"].monitors,
            ["material"]
        );
        assert!(compilation.report.monitored_metrics.iter().any(|metric| {
            metric.path == "Factory.leaf.material"
                && metric.ty == "Mass"
                && metric.value == "2 Kilogram"
                && metric.aggregate == "sum"
        }));
        let report = serde_json::to_value(&compilation.report).expect("JSON report");
        let factory = report["monitored_metrics"]
            .as_array()
            .expect("monitored metrics")
            .iter()
            .find(|metric| metric["path"] == "Factory.material")
            .expect("factory metric JSON");
        assert!(factory.get("direct_value").is_none());
    }

    #[test]
    fn monitored_aggregate_requires_child_coverage() {
        let compilation = check_source(
            r#"
                process MassAccounting {
                    export material: Mass aggregate sum
                }
                process Bare { () => {} }
                process Factory: MassAccounting {
                    monitor material
                    () => {
                        let bare_process = Bare()
                        let bare = bare_process()
                    }
                }
            "#,
        );
        assert_eq!(compilation.report.status, Completeness::Partial);
        assert!(compilation.report.obligations.iter().any(|obligation| {
            obligation.kind == "monitored_metric_coverage"
                && obligation.path == "Factory.bare.material"
        }));
        assert!(compilation.report.assumed_zero.is_empty());
    }

    #[test]
    fn monitoring_propagates_through_nested_aggregates() {
        let compilation = check_source(
            r#"
                process MassAccounting {
                    export material: Mass aggregate sum
                }
                process Bare { () => {} }
                process Inner: MassAccounting {
                    () => {
                        let bare_process = Bare()
                        let bare = bare_process()
                    }
                }
                process Outer: MassAccounting {
                    monitor material
                    () => {
                        let inner_process = Inner()
                        let inner = inner_process()
                    }
                }
            "#,
        );
        assert_eq!(compilation.report.status, Completeness::Partial);
        assert!(compilation.report.obligations.iter().any(|obligation| {
            obligation.kind == "monitored_metric_coverage"
                && obligation.path == "Outer.inner.bare.material"
        }));
        assert!(compilation.report.assumed_zero.is_empty());
    }

    /// `monitor` sits beside the inheritable member declarations, so it inherits
    /// along the extends chain. Declaring it once on a contract used to be
    /// silently inert: the metric vanished from the report with no diagnostic,
    /// while `status` stayed complete and obligations stayed empty.
    #[test]
    fn monitor_inherits_from_a_contract() {
        let compilation = check_source(
            r#"
                process MassAccounting {
                    export material: Mass aggregate sum
                    monitor material as Gram
                }
                process Leaf: MassAccounting {
                    () => { material = 2 Kilogram }
                }
                process Outer {
                    () => {
                        let leaf_process = Leaf()
                        let leaf = leaf_process()
                    }
                }
            "#,
        );
        assert_eq!(monitored(&compilation, "Outer.leaf.material"), "2000 Gram");
    }

    /// A local clause overrides an inherited one, matching the way a local
    /// assignment replaces an inherited body fallback.
    #[test]
    fn a_local_monitor_overrides_an_inherited_display_unit() {
        let compilation = check_source(
            r#"
                process MassAccounting {
                    export material: Mass aggregate sum
                    monitor material as Gram
                }
                process Leaf: MassAccounting {
                    monitor material as Kilogram
                    () => { material = 2 Kilogram }
                }
                process Outer {
                    () => {
                        let leaf_process = Leaf()
                        let leaf = leaf_process()
                    }
                }
            "#,
        );
        assert_eq!(monitored(&compilation, "Outer.leaf.material"), "2 Kilogram");
    }

    #[test]
    fn nested_aggregate_values_are_fully_instantiated_and_flattened() {
        let compilation = check_source(
            r#"
                process MassAccounting {
                    export material: Mass aggregate sum
                }
                process Leaf: MassAccounting {
                    () => { material = 2 Kilogram }
                }
                process Inner: MassAccounting {
                    () => {
                        let leaf_process = Leaf()
                        let leaf = leaf_process()
                    }
                }
                process Outer: MassAccounting {
                    monitor material
                    () => {
                        let inner_process = Inner()
                        let inner = inner_process()
                    }
                }
            "#,
        );
        assert_eq!(compilation.report.status, Completeness::Complete);
        let metric = compilation
            .report
            .monitored_metrics
            .iter()
            .find(|metric| metric.path == "Outer.material")
            .expect("outer aggregate metric");
        assert_eq!(metric.value, "2 Kilogram", "{metric:?}");
        assert!(metric.expression.is_none(), "{metric:?}");
        assert!(
            compilation
                .report
                .monitored_metrics
                .iter()
                .all(|metric| !metric.value.contains("sum(sum("))
        );
    }

    #[test]
    fn monitored_aggregates_fold_exact_decimal_quantities() {
        let compilation = check_source(
            r#"
                process CostAccounting {
                    export cost: USD aggregate sum
                }
                process First: CostAccounting {
                    () => { cost = 1.25 USD }
                }
                process Second: CostAccounting {
                    () => { cost = 2 USD }
                }
                process Factory: CostAccounting {
                    monitor cost
                    () => {
                        let first_process = First()
                        let first = first_process()
                        let second_process = Second()
                        let second = second_process()
                    }
                }
            "#,
        );
        assert_eq!(compilation.report.status, Completeness::Complete);
        assert!(compilation.report.monitored_metrics.iter().any(|metric| {
            metric.path == "Factory.cost"
                && metric.value == "3.25 USD"
                && metric.aggregate == "sum"
                && metric.dependencies.is_empty()
                && metric.expression.is_some()
        }));
    }

    #[test]
    fn aggregate_assignments_are_direct_contributions_not_total_overrides() {
        let compilation = check_source(
            r#"
                process CostAccounting {
                    export cost: USD aggregate sum
                }
                process Leaf: CostAccounting {
                    () => { cost = 2 USD }
                }
                process Factory: CostAccounting {
                    monitor cost
                    () => {
                        cost = 5 USD
                        let leaf = Leaf()()
                    }
                }
            "#,
        );
        assert_eq!(compilation.report.status, Completeness::Complete);
        let metric = compilation
            .report
            .monitored_metrics
            .iter()
            .find(|metric| metric.path == "Factory.cost")
            .expect("factory total");
        assert_eq!(metric.value, "7 USD", "{metric:?}");
        assert_eq!(metric.direct_value.as_deref(), Some("5 USD"));
        assert_eq!(metric.expression.as_deref(), Some("sum(5 USD, 2 USD)"));
        assert!(compilation.report.constraints.iter().any(|constraint| {
            constraint.path == "Factory.completion.aggregate[cost]"
                && constraint.predicate == "cost == sum(5 USD, leaf.cost)"
        }));
        assert_eq!(
            compilation.model.processes["Factory"].aggregate_contributions["cost"],
            "5 USD"
        );
        let report = serde_json::to_value(&compilation.report).expect("JSON report");
        let factory = report["monitored_metrics"]
            .as_array()
            .expect("monitored metrics")
            .iter()
            .find(|metric| metric["path"] == "Factory.cost")
            .expect("factory metric JSON");
        assert_eq!(factory["direct_value"], "5 USD");
        assert!(factory.get("direct_expression").is_none());
    }

    #[test]
    fn inherited_aggregate_definitions_are_overridable_fallbacks() {
        let compilation = check_source(
            r#"
                process CostAccounting {
                    export cost: USD aggregate sum
                }
                process ZeroCost: CostAccounting {
                    () => { cost = 0 USD }
                }
                process Free: ZeroCost {
                    () => { requires true }
                }
                process Paid: ZeroCost {
                    () => { cost = 3 USD }
                }
                process Factory: CostAccounting {
                    monitor cost
                    () => {
                        let free = Free()()
                        let paid = Paid()()
                    }
                }
            "#,
        );
        assert_eq!(compilation.report.status, Completeness::Complete);
        let by_path = compilation
            .report
            .monitored_metrics
            .iter()
            .map(|metric| (metric.path.as_str(), metric))
            .collect::<HashMap<_, _>>();
        assert_eq!(by_path["Factory.cost"].value, "3 USD");
        assert_eq!(
            by_path["Factory.free.cost"].direct_value.as_deref(),
            Some("0 USD")
        );
        assert_eq!(
            by_path["Factory.paid.cost"].direct_value.as_deref(),
            Some("3 USD")
        );
    }

    #[test]
    fn inherited_fallbacks_cross_bodyless_aliases_and_close_that_export() {
        let compilation = check_source(
            r#"
                process CostAccounting {
                    export cost: USD aggregate sum
                }
                process ZeroCost: CostAccounting {
                    () => { cost = 0 USD }
                }
                process Alias: ZeroCost {}
                process Factory: CostAccounting {
                    monitor cost
                    () => {
                        let alias = Alias()()
                    }
                }
            "#,
        );
        assert_eq!(
            compilation.report.status,
            Completeness::Complete,
            "{}",
            compilation.render_human()
        );
        assert!(!compilation.report.obligations.iter().any(|obligation| {
            obligation.path == "Factory.alias.cost" && obligation.kind == "open_process_export"
        }));
        let alias = compilation
            .report
            .monitored_metrics
            .iter()
            .find(|metric| metric.path == "Factory.alias.cost")
            .expect("bodyless alias fallback");
        assert_eq!(alias.value, "0 USD");
        assert_eq!(alias.direct_value.as_deref(), Some("0 USD"));
    }

    #[test]
    fn inherited_fallbacks_may_depend_on_inherited_configuration() {
        let compilation = check_source(
            r#"
                process CostAccounting {
                    export cost: USD aggregate sum
                }
                process Priced: CostAccounting {
                    config price: USD
                    () => { cost = price }
                }
                process Alias: Priced {}
                process Factory: CostAccounting {
                    monitor cost
                    () => {
                        let alias = Alias(price: 4 USD)()
                    }
                }
            "#,
        );
        assert_eq!(
            compilation.report.status,
            Completeness::Complete,
            "{}",
            compilation.render_human()
        );
        assert!(compilation.report.monitored_metrics.iter().any(|metric| {
            metric.path == "Factory.alias.cost"
                && metric.value == "4 USD"
                && metric.direct_value.as_deref() == Some("4 USD")
        }));
    }

    #[test]
    fn inherited_aggregate_fallback_applies_on_unassigned_symbolic_paths() {
        let compilation = check_source(
            r#"
                process CostAccounting {
                    export cost: USD aggregate sum
                }
                process ZeroCost: CostAccounting {
                    () => { cost = 0 USD }
                }
                process Conditional: ZeroCost {
                    monitor cost
                    (enabled: Bool) => {
                        if enabled {
                            cost = 2 USD
                        }
                    }
                }
            "#,
        );
        assert_eq!(compilation.report.status, Completeness::Complete);
        assert!(!compilation.report.obligations.iter().any(|obligation| {
            obligation.kind == "missing_export" && obligation.path == "Conditional.cost"
        }));
        let metric = compilation
            .report
            .monitored_metrics
            .iter()
            .find(|metric| metric.path == "Conditional.cost")
            .expect("conditional cost");
        assert_eq!(metric.value, "if_then_else(enabled, 2 USD, 0 USD)");
        assert_eq!(metric.direct_value, metric.value.clone().into());
    }

    #[test]
    fn conflicting_inherited_aggregate_fallbacks_are_rejected() {
        let compilation = check_source(
            r#"
                process CostAccounting {
                    export cost: USD aggregate sum
                }
                process One: CostAccounting {
                    () => { cost = 1 USD }
                }
                process Two: CostAccounting {
                    () => { cost = 2 USD }
                }
                process Ambiguous: One, Two {
                    () => { requires true }
                }
            "#,
        );
        assert_eq!(compilation.report.status, Completeness::Invalid);
        assert!(compilation.report.diagnostics.iter().any(|diagnostic| {
            diagnostic.code == "E0513"
                && diagnostic
                    .message
                    .contains("conflicting inherited definitions")
        }));
    }

    #[test]
    fn equivalent_inherited_fallbacks_coalesce_after_unit_normalization() {
        let compilation = check_source(
            r#"
                unit ThousandUSD = USD * 1000
                process CostAccounting {
                    export cost: USD aggregate sum
                }
                process Dollars: CostAccounting {
                    () => { cost = 1000 USD }
                }
                process Thousands: CostAccounting {
                    () => { cost = 1 ThousandUSD }
                }
                process Coalesced: Dollars, Thousands {
                    monitor cost
                    () => { requires true }
                }
            "#,
        );
        assert_eq!(
            compilation.report.status,
            Completeness::Complete,
            "{}",
            compilation.render_human()
        );
        assert!(
            !compilation
                .report
                .diagnostics
                .iter()
                .any(|diagnostic| diagnostic.code == "E0513")
        );
        assert!(
            compilation
                .report
                .monitored_metrics
                .iter()
                .any(|metric| { metric.path == "Coalesced.cost" && metric.value == "1000 USD" })
        );
    }

    #[test]
    fn direct_aggregate_contribution_cannot_recursively_read_derived_total() {
        let recursive = check_source(
            r#"
                process CostAccounting {
                    export cost: USD aggregate sum
                }
                process Recursive: CostAccounting {
                    () => { cost = cost + 1 USD }
                }
            "#,
        );
        assert_eq!(recursive.report.status, Completeness::Invalid);
        assert!(recursive.report.diagnostics.iter().any(|diagnostic| {
            diagnostic.code == "E0627" && diagnostic.message.contains("derived total")
        }));

        let inherited = check_source(
            r#"
                process CostAccounting {
                    export cost: USD aggregate sum
                }
                process ZeroCost: CostAccounting {
                    () => { cost = 0 USD }
                }
                process TwoMore: ZeroCost {
                    monitor cost
                    () => { cost = cost + 2 USD }
                }
            "#,
        );
        assert_eq!(
            inherited.report.status,
            Completeness::Complete,
            "{}",
            inherited.render_human()
        );
        assert!(inherited.report.monitored_metrics.iter().any(|metric| {
            metric.path == "TwoMore.cost"
                && metric.value == "2 USD"
                && metric.direct_value.as_deref() == Some("2 USD")
        }));
    }

    #[test]
    fn direct_contributions_cannot_absorb_automatic_child_totals() {
        let compilation = check_source(
            r#"
                process CostAccounting {
                    export cost: USD aggregate sum
                }
                process Leaf: CostAccounting {
                    () => { cost = 2 USD }
                }
                process Absorber: CostAccounting {
                    () => {
                        let leaf = Leaf()()
                        cost = leaf.cost
                    }
                }
            "#,
        );
        assert_eq!(compilation.report.status, Completeness::Invalid);
        assert!(compilation.report.diagnostics.iter().any(|diagnostic| {
            diagnostic.code == "E0628"
                && diagnostic
                    .message
                    .contains("aggregation includes them automatically")
        }));
    }

    #[test]
    fn inherited_fallbacks_reject_non_inherited_dependencies() {
        let compilation = check_source(
            r#"
                process CostAccounting {
                    export cost: USD aggregate sum
                }
                process PerInvocation: CostAccounting {
                    (amount: USD) => { cost = amount }
                }
                process MissingAmount: PerInvocation {
                    () => { requires true }
                }
            "#,
        );
        assert_eq!(compilation.report.status, Completeness::Invalid);
        assert!(compilation.report.diagnostics.iter().any(|diagnostic| {
            diagnostic.code == "E0514" && diagnostic.message.contains("amount")
        }));

        let wrong_type = check_source(
            r#"
                process CostAccounting {
                    export cost: USD aggregate sum
                }
                process Parent: CostAccounting {
                    (amount: USD) => { cost = amount }
                }
                process Child: Parent {
                    monitor cost
                    (amount: Meter) => { requires true }
                }
            "#,
        );
        assert_eq!(wrong_type.report.status, Completeness::Invalid);
        assert!(
            wrong_type.report.diagnostics.iter().any(|diagnostic| {
                diagnostic.code == "E0514" && diagnostic.message.contains("parent USD, child Meter")
            }),
            "{}",
            wrong_type.render_human()
        );
    }

    #[test]
    fn explicit_aggregate_may_define_a_manual_child_derived_total() {
        let compilation = check_source(
            r#"
                process AutomaticCost {
                    export cost: USD aggregate sum
                }
                process ManualCost {
                    export cost: USD aggregate explicit
                }
                process Leaf: AutomaticCost {
                    () => { cost = 2 USD }
                }
                process Manual: ManualCost {
                    monitor cost
                    () => {
                        let leaf = Leaf()()
                        cost = leaf.cost
                    }
                }
            "#,
        );
        assert_eq!(
            compilation.report.status,
            Completeness::Complete,
            "{}",
            compilation.render_human()
        );
        assert!(
            !compilation
                .report
                .diagnostics
                .iter()
                .any(|diagnostic| diagnostic.code == "E0628")
        );
        assert!(
            compilation.report.monitored_metrics.iter().any(|metric| {
                metric.path == "Manual.cost"
                    && metric.value == "2 USD"
                    && metric.aggregate == "explicit"
            }),
            "{}",
            compilation.render_human()
        );
    }

    #[test]
    fn composite_direct_duration_is_serialized_with_child_dag_makespan() {
        let compilation = check_source(
            r#"
                process DurationAccounting {
                    export duration: Second aggregate makespan
                }
                process TwoSeconds: DurationAccounting {
                    () => { duration = 2 Second }
                }
                process ThreeSeconds: DurationAccounting {
                    () => { duration = 3 Second }
                }
                process FiveSeconds: DurationAccounting {
                    () => { duration = 5 Second }
                }
                process Factory: DurationAccounting {
                    monitor duration
                    () => {
                        duration = 1 Second
                        let first = TwoSeconds()()
                        let parallel = ThreeSeconds()()
                        let last = FiveSeconds()()
                        first before last
                        parallel before last
                    }
                }
            "#,
        );
        assert_eq!(
            compilation.report.status,
            Completeness::Complete,
            "{:?}",
            compilation.report.diagnostics
        );
        let metric = compilation
            .report
            .monitored_metrics
            .iter()
            .find(|metric| metric.path == "Factory.duration")
            .expect("factory duration");
        assert_eq!(metric.value, "9 Second", "{metric:?}");
        assert_eq!(metric.direct_value.as_deref(), Some("1 Second"));
        assert!(metric.bounds.is_none(), "{metric:?}");
    }

    #[test]
    fn nested_composites_add_only_direct_contribution_to_direct_child_totals() {
        let compilation = check_source(
            r#"
                process CostAccounting {
                    export cost: USD aggregate sum
                }
                process Leaf: CostAccounting {
                    () => { cost = 2 USD }
                }
                process Inner: CostAccounting {
                    () => {
                        cost = 1 USD
                        let leaf = Leaf()()
                    }
                }
                process Outer: CostAccounting {
                    monitor cost
                    () => {
                        cost = 4 USD
                        let inner = Inner()()
                    }
                }
            "#,
        );
        assert_eq!(compilation.report.status, Completeness::Complete);
        let by_path = compilation
            .report
            .monitored_metrics
            .iter()
            .map(|metric| (metric.path.as_str(), metric))
            .collect::<HashMap<_, _>>();
        assert_eq!(by_path["Outer.cost"].value, "7 USD");
        assert_eq!(by_path["Outer.cost"].direct_value.as_deref(), Some("4 USD"));
        assert_eq!(by_path["Outer.inner.cost"].value, "3 USD");
        assert_eq!(
            by_path["Outer.inner.cost"].direct_value.as_deref(),
            Some("1 USD")
        );
        assert_eq!(by_path["Outer.inner.leaf.cost"].value, "2 USD");
    }

    #[test]
    fn stream_connected_duration_makespan_remains_symbolic() {
        let compilation = check_source(
            r#"
                artifact Token {}
                process DurationAccounting {
                    export duration: Second aggregate makespan
                }
                process Producer: DurationAccounting {
                    export tokens: Stream[Token]
                    () => {
                        duration = 2 Second
                        yield tokens = Token()
                    }
                }
                process Consumer: DurationAccounting {
                    gets tokens: Stream[Token]
                    () => {
                        duration = 3 Second
                        for token in tokens {
                            requires token == token
                        }
                    }
                }
                process Factory: DurationAccounting {
                    monitor duration
                    () => {
                        duration = 1 Second
                        let producer = Producer()()
                        let consumer = Consumer()()
                        producer.tokens -> consumer.tokens
                    }
                }
            "#,
        );
        assert_eq!(
            compilation.report.status,
            Completeness::Complete,
            "{:?}",
            compilation.report.diagnostics
        );
        let metric = compilation
            .report
            .monitored_metrics
            .iter()
            .find(|metric| metric.path == "Factory.duration")
            .expect("factory duration");
        assert!(metric.value.contains("makespan("), "{metric:?}");
        assert_eq!(metric.direct_value.as_deref(), Some("1 Second"));
        let bounds = metric.bounds.as_ref().expect("pipeline duration bounds");
        assert_eq!(bounds.lower, "4 Second");
        assert_eq!(bounds.upper, "6 Second");
        assert_eq!(bounds.reason, "unresolved pipeline makespan");
        assert!(
            compilation
                .render_human()
                .contains("  Factory.duration:\n    4–6 Second\n    unresolved pipeline makespan")
        );
        let temporal = compilation
            .report
            .temporal_metrics
            .iter()
            .find(|metric| metric.path == "Factory.duration")
            .expect("temporal duration projection");
        assert_eq!(temporal.status, "bounded");
        assert_eq!(temporal.known_work.lower, "4 Second");
        assert_eq!(temporal.known_work.upper, "6 Second");
        assert_eq!(temporal.known_work.lower_seconds.as_deref(), Some("4"));
        assert_eq!(temporal.known_work.upper_seconds.as_deref(), Some("6"));
        assert_eq!(temporal.completion_lower.as_deref(), Some("4 Second"));
        assert_eq!(temporal.completion_upper.as_deref(), Some("6 Second"));
        assert!(temporal.obligations.is_empty());
    }

    #[test]
    fn projects_stream_first_emission_events_through_a_run() {
        let compilation = check_source(
            r#"
                artifact Token {}
                process Producer {
                    export tokens: Stream[Token]
                    (first_item_delay: Time) => {
                        requires cardinality(tokens) == 3
                        yield tokens = Token() after first_item_delay
                    }
                }
                process Factory {
                    export tokens: Stream[Token]
                    () => {
                        let producer = Producer()(first_item_delay: 2 Second)
                        producer.tokens -> tokens
                    }
                }
            "#,
        );
        assert_eq!(compilation.report.status, Completeness::Complete);
        let handoff = compilation
            .report
            .stream_handoffs
            .iter()
            .find(|handoff| handoff.path == "Factory.producer" && handoff.stream == "tokens")
            .expect("instantiated stream handoff");
        assert_eq!(handoff.first_item_after, "2 Second");
        assert_eq!(handoff.first_item_after_seconds.as_deref(), Some("2"));
        assert_eq!(handoff.status, "exact");
        let cardinality = compilation
            .report
            .stream_cardinalities
            .iter()
            .find(|cardinality| {
                cardinality.path == "Factory.producer" && cardinality.stream == "tokens"
            })
            .expect("instantiated stream cardinality");
        assert_eq!(cardinality.cardinality, "3");
        assert_eq!(cardinality.status, "exact");
    }

    #[test]
    fn rejects_non_time_stream_first_emission_events() {
        let compilation = check_source(
            r#"
                artifact Token {}
                process Producer {
                    export tokens: Stream[Token]
                    () => { yield tokens = Token() after 2 }
                }
            "#,
        );
        assert!(
            compilation
                .report
                .diagnostics
                .iter()
                .any(|diagnostic| diagnostic.code == "E0618")
        );
    }

    #[test]
    fn missing_monitored_duration_does_not_create_a_completion_bound() {
        let compilation = check_source(
            r#"
                process DurationAccounting {
                    export duration: Second aggregate makespan
                }
                process Timed: DurationAccounting {
                    () => { duration = 2 Second }
                }
                process Untimed {}
                process Factory: DurationAccounting {
                    monitor duration
                    () => {
                        let timed = Timed()()
                        let untimed = Untimed()()
                    }
                }
            "#,
        );
        assert_eq!(compilation.report.status, Completeness::Partial);
        let temporal = compilation
            .report
            .temporal_metrics
            .iter()
            .find(|metric| metric.path == "Factory.duration")
            .expect("temporal duration projection");
        assert_eq!(temporal.status, "incomplete");
        assert_eq!(temporal.known_work.lower, "2 Second");
        assert_eq!(temporal.known_work.upper, "2 Second");
        assert_eq!(temporal.known_work.lower_seconds.as_deref(), Some("2"));
        assert_eq!(temporal.known_work.upper_seconds.as_deref(), Some("2"));
        assert!(temporal.completion_lower.is_none());
        assert!(temporal.completion_upper.is_none());
        assert_eq!(temporal.obligations, vec!["Factory.untimed.duration"]);
        assert!(compilation.render_human().contains(
            "Factory.duration:\n    known work: 2–2 Second\n    successful completion: unresolved (1 open duration obligation)"
        ));
    }

    #[test]
    fn unordered_exclusive_loans_keep_duration_makespan_symbolic() {
        let compilation = check_source(
            r#"
                artifact Tool { state: Integer }
                process DurationAccounting {
                    export duration: Second aggregate makespan
                }
                process Observe: DurationAccounting {
                    uses tool: Tool
                    (duration_value: Second) => {
                        duration = duration_value
                        requires tool.state >= 0
                    }
                }
                process Factory: DurationAccounting {
                    gets tool: Tool
                    monitor duration
                    () => {
                        let observer = Observe()
                        let first = observer(duration_value: 2 Second)
                        let second = observer(duration_value: 3 Second)
                        tool -> first.tool
                        tool -> second.tool
                    }
                }
            "#,
        );
        assert_eq!(
            compilation.report.status,
            Completeness::Complete,
            "{}",
            compilation.render_human()
        );
        assert!(compilation.report.constraints.iter().any(|constraint| {
            constraint.phase == "scheduling" && constraint.predicate.contains("non_overlap")
        }));
        let metric = compilation
            .report
            .monitored_metrics
            .iter()
            .find(|metric| metric.path == "Factory.duration")
            .expect("factory duration");
        assert!(metric.value.contains("makespan("), "{metric:?}");
        assert_ne!(metric.value, "3 Second");
        let bounds = metric.bounds.as_ref().expect("scheduling bounds");
        assert_eq!(bounds.lower, "3 Second");
        assert_eq!(bounds.upper, "5 Second");
        assert_eq!(bounds.reason, "unresolved scheduling makespan");
    }

    #[test]
    fn monitored_metrics_use_compatible_author_defined_display_units() {
        let compilation = check_source(
            r#"
                unit MillionUSD = USD * 1000000
                unit Thousand = Scalar * 1000
                process CostAccounting {
                    export cost: USD aggregate sum
                }
                process Leaf: CostAccounting {
                    () => { cost = 2500000 USD }
                }
                process Inner: CostAccounting {
                    monitor cost as USD
                    () => {
                        let leaf_process = Leaf()
                        let leaf = leaf_process()
                    }
                }
                process Outer: CostAccounting {
                    monitor cost as MillionUSD
                    () => {
                        let inner_process = Inner()
                        let inner = inner_process()
                    }
                }
                process Counter {
                    export completed_jobs: Integer aggregate sum
                    monitor completed_jobs as Thousand
                    () => { completed_jobs = 2500 }
                }
            "#,
        );
        assert_eq!(compilation.report.status, Completeness::Complete);
        for path in ["Outer.cost", "Outer.inner.cost", "Outer.inner.leaf.cost"] {
            let metric = compilation
                .report
                .monitored_metrics
                .iter()
                .find(|metric| metric.path == path)
                .unwrap_or_else(|| panic!("missing metric {path}"));
            assert_eq!(metric.value, "2.5 MillionUSD");
            assert_eq!(metric.display_unit.as_deref(), Some("MillionUSD"));
        }
        let stored = compilation
            .report
            .monitored_metrics
            .iter()
            .find(|metric| metric.path == "Counter.completed_jobs")
            .expect("completed job metric");
        assert_eq!(stored.value, "2.5 Thousand");
        assert_eq!(stored.display_unit.as_deref(), Some("Thousand"));
        assert_eq!(
            compilation.model.processes["Outer"].monitor_display_units["cost"],
            "MillionUSD"
        );
    }

    #[test]
    fn monitor_display_units_must_exist_and_match_the_metric_dimension() {
        let compilation = check_source(
            r#"
                process CostAccounting {
                    export cost: USD aggregate sum
                }
                process UnknownDisplay: CostAccounting {
                    monitor cost as MissingCurrency
                    () => { cost = 1 USD }
                }
                process WrongDimension: CostAccounting {
                    monitor cost as Meter
                    () => { cost = 1 USD }
                }
            "#,
        );
        assert_eq!(compilation.report.status, Completeness::Invalid);
        assert!(
            compilation
                .report
                .diagnostics
                .iter()
                .any(|diagnostic| diagnostic.code == "E0511")
        );
        assert!(
            compilation
                .report
                .diagnostics
                .iter()
                .any(|diagnostic| diagnostic.code == "E0512")
        );
    }

    #[test]
    fn nominal_base_units_type_check_and_convert_exactly() {
        let valid = check_source(
            r#"
                unit Information = base
                unit Byte = Information
                unit Bit = Byte / 8
                unit Petabyte = Byte * 1000000000000000
                unit Gigabit = Bit * 1000000000
                unit GigabitPerSecond = Gigabit / Second

                process DataAccounting {
                    export data: Byte aggregate sum
                    export rate: GigabitPerSecond aggregate max
                    monitor data as Petabyte
                    monitor rate as GigabitPerSecond

                    () => {
                        requires 1 Byte == 8 Bit
                        data = 3 Petabyte
                        rate = 7 GigabitPerSecond
                    }
                }
            "#,
        );
        assert_eq!(valid.report.status, Completeness::Complete);
        assert!(valid.report.monitored_metrics.iter().any(|metric| {
            metric.path == "DataAccounting.data"
                && metric.ty == "Byte"
                && metric.value == "3 Petabyte"
        }));
        assert!(valid.report.monitored_metrics.iter().any(|metric| {
            metric.path == "DataAccounting.rate"
                && metric.ty == "GigabitPerSecond"
                && metric.value == "7 GigabitPerSecond"
        }));

        let invalid = check_source(
            r#"
                unit Information = base
                unit Byte = Information
                process Bad {
                    () => { requires 1 Byte == 1 }
                }
            "#,
        );
        assert_eq!(invalid.report.status, Completeness::Invalid);
        assert!(
            invalid
                .report
                .diagnostics
                .iter()
                .any(|diagnostic| diagnostic.code == "E0808")
        );
    }

    #[test]
    fn inline_dimensional_types_and_bare_units_are_exact_quantities() {
        let compilation = check_source(
            r#"
                unit MeterPerSecond = Meter / Second

                process Result {
                    monitor velocity: Length / Time as MeterPerSecond

                    () => {
                        velocity = Meter / Second
                    }
                }
            "#,
        );
        assert_eq!(
            compilation.report.status,
            Completeness::Complete,
            "{:?}",
            compilation.report.diagnostics
        );
        assert!(compilation.report.monitored_metrics.iter().any(|metric| {
            metric.path == "Result.velocity"
                && metric.ty == "Length / Time"
                && metric.value == "1 MeterPerSecond"
                && metric.aggregate == "explicit"
        }));
    }

    #[test]
    fn inline_concrete_unit_types_set_the_default_metric_presentation() {
        let compilation = check_source(
            r#"
                unit Mile = Meter * 1609.344
                unit Minute = Second * 60
                unit Hour = Minute * 60

                process Result {
                    export rate: Mile / Hour aggregate sum
                    monitor rate

                    () => {
                        rate = 1 Mile / Second
                    }
                }
            "#,
        );
        assert_eq!(
            compilation.report.status,
            Completeness::Complete,
            "{:?}",
            compilation.report.diagnostics
        );
        assert!(compilation.report.monitored_metrics.iter().any(|metric| {
            metric.path == "Result.rate"
                && metric.ty == "Mile / Hour"
                && metric.value == "3600 Mile / Hour"
        }));
    }

    #[test]
    fn monitored_composite_unit_quantities_simplify_without_a_named_alias() {
        let compilation = check_source(
            r#"
                process Result {
                    monitor rate: Length / Time

                    () => {
                        rate = (1 Meter / 1 Second) + (1 Meter / 12 Second)
                    }
                }
            "#,
        );
        assert_eq!(
            compilation.report.status,
            Completeness::Complete,
            "{:?}",
            compilation.report.diagnostics
        );
        assert!(compilation.report.monitored_metrics.iter().any(|metric| {
            metric.path == "Result.rate"
                && metric.value == "13/12 Meter / Second"
                && metric.expression.is_some()
        }));
    }

    #[test]
    fn monitored_quantities_collapse_to_one_compatible_author_unit() {
        let compilation = check_source(
            r#"
                unit Information = base
                unit Byte = Information
                unit Gigabyte = Byte * 1000000000
                unit Hour = Second * 3600

                process Result {
                    monitor transferred: Information

                    () => {
                        let rate = 1 Gigabyte / Second
                        transferred = rate * 5 Hour
                    }
                }
            "#,
        );
        assert_eq!(
            compilation.report.status,
            Completeness::Complete,
            "{:?}",
            compilation.report.diagnostics
        );
        assert!(compilation.report.monitored_metrics.iter().any(|metric| {
            metric.path == "Result.transferred" && metric.value == "18000 Gigabyte"
        }));
    }

    #[test]
    fn monitored_composite_unit_quantities_cancel_powers_before_display() {
        let compilation = check_source(
            r#"
                unit Speed = Length / Time
                unit Centimeter = Meter / 100
                unit Minute = Second * 60
                unit Hour = Minute * 60

                process Result {
                    export rate: Speed aggregate sum
                    monitor rate

                    () => {
                        let volume = 1 Centimeter^3
                        let duration = 5 Hour
                        let length = volume / (1 Meter^2)
                        rate = length / duration
                    }
                }
            "#,
        );
        assert_eq!(
            compilation.report.status,
            Completeness::Complete,
            "{:?}",
            compilation.report.diagnostics
        );
        assert!(compilation.report.monitored_metrics.iter().any(|metric| {
            metric.path == "Result.rate"
                && metric.value == "0.00002 Centimeter / Hour"
                && metric.expression.is_some()
        }));
    }

    #[test]
    fn monitored_metrics_fold_exact_powers_and_ceiling() {
        let compilation = check_source(
            r#"
                import math
                process Counter {
                    export samples: Integer aggregate sum
                    monitor samples
                    () => {
                        samples = math.ceil((2^3) / 3)
                    }
                }
            "#,
        );
        assert_eq!(compilation.report.status, Completeness::Complete);
        assert!(compilation.report.monitored_metrics.iter().any(|metric| {
            metric.path == "Counter.samples"
                && metric.value == "3"
                && metric.dependencies.is_empty()
        }));
    }

    #[test]
    fn rounding_preserves_the_resolved_quantity_scale() {
        let compilation = check_source(
            r#"
                import math
                unit Millimeter = Meter / 1000
                unit Information = base
                unit Byte = Information

                process Rounded {
                    export length_up: Millimeter aggregate explicit
                    export length_down: Millimeter aggregate explicit
                    export bytes: Byte aggregate explicit
                    export count: Integer aggregate explicit
                    monitor length_up
                    monitor length_down
                    monitor bytes
                    monitor count

                    () => {
                        length_up = math.ceil(1.2 Millimeter)
                        length_down = math.floor(-1.2 Millimeter)
                        bytes = math.ceil(1200.1 Byte / 2)
                        count = math.ceil(1 Meter / 3 Millimeter)
                    }
                }
            "#,
        );
        assert_eq!(
            compilation.report.status,
            Completeness::Complete,
            "{}",
            compilation.render_human()
        );
        for (path, value) in [
            ("Rounded.length_up", "2 Millimeter"),
            ("Rounded.length_down", "-2 Millimeter"),
            ("Rounded.bytes", "601 Byte"),
            ("Rounded.count", "334"),
        ] {
            assert!(
                compilation
                    .report
                    .monitored_metrics
                    .iter()
                    .any(|metric| metric.path == path && metric.value == value)
            );
        }
    }

    #[test]
    fn rejects_unknown_intrinsics_and_nonnumeric_arithmetic() {
        let compilation = check_source(
            "process P { () => { requires missing.length() > 0 requires true + false } }\n",
        );
        assert_eq!(compilation.report.status, Completeness::Invalid);
        assert!(
            compilation
                .report
                .diagnostics
                .iter()
                .any(|diagnostic| diagnostic.code == "E0811")
        );
        assert!(
            compilation
                .report
                .diagnostics
                .iter()
                .any(|diagnostic| diagnostic.code == "E0809")
        );

        let compilation =
            check_source("import math\nprocess P { () => { requires math.sum(1) > 0 } }\n");
        assert_eq!(compilation.report.status, Completeness::Invalid);
        assert!(
            compilation
                .report
                .diagnostics
                .iter()
                .any(|diagnostic| diagnostic.code == "E0819")
        );

        let compilation = check_source(
            "import math\nprocess P { () => { requires math.ceil(true) > 0 requires math.sqrt(false) > 0 requires true < false } }\n",
        );
        assert_eq!(compilation.report.status, Completeness::Invalid);
        for code in ["E0805", "E0822", "E0808"] {
            assert!(
                compilation
                    .report
                    .diagnostics
                    .iter()
                    .any(|diagnostic| diagnostic.code == code),
                "missing diagnostic {code}"
            );
        }

        let compilation = check_source("process P { () => { requires all_same([1, 1]) } }\n");
        assert_eq!(compilation.report.status, Completeness::Invalid);
        assert!(
            compilation
                .report
                .diagnostics
                .iter()
                .any(|diagnostic| diagnostic.code == "E0813")
        );
    }

    #[test]
    fn rejects_reversed_parent_and_child_wire_roles() {
        let compilation = check_source(
            r#"
                artifact A {}
                process Child {
                    gets input: A
                    export output: A
                    () => {}
                }
                process Parent {
                    gets input: A
                    export output: A
                    () => {
                        let configured = Child()
                        let run = configured()
                        output -> run.input
                        run.output -> input
                    }
                }
            "#,
        );
        assert_eq!(compilation.report.status, Completeness::Invalid);
        assert!(
            compilation
                .report
                .diagnostics
                .iter()
                .any(|diagnostic| diagnostic.code == "E0708")
        );
        assert!(
            compilation
                .report
                .diagnostics
                .iter()
                .any(|diagnostic| diagnostic.code == "E0709")
        );
    }

    #[test]
    fn validates_pure_function_bodies_and_named_results() {
        let compilation = check_source(
            r#"
                artifact A { mass: Mass }
                fn wrong(value: Integer) -> (result: Bool) {
                    result = value
                }
                fn incomplete(flag: Bool) -> (result: Integer) {
                    if flag { result = 1 }
                }
                fn mutates(value: A) -> (result: Integer) {
                    value.mass = 1 Kilogram
                    result = 1
                }
                fn constructs() -> (result: Bool) {
                    result = A(mass: 1 Kilogram) == A(mass: 1 Kilogram)
                }
            "#,
        );
        assert_eq!(compilation.report.status, Completeness::Invalid);
        for code in ["E0415", "E0407", "E0413", "E0426"] {
            assert!(
                compilation
                    .report
                    .diagnostics
                    .iter()
                    .any(|diagnostic| diagnostic.code == code),
                "missing diagnostic {code}"
            );
        }
    }

    #[test]
    fn checks_generic_function_calls_and_copyable_bounds() {
        let valid = check_source(
            r#"
                fn identity[T](value: T) -> (result: T)
                where T: Copyable {
                    result = value
                }
                process P {
                    () => {
                        let answer = identity(value: 1)
                        requires answer.result == 1
                    }
                }
            "#,
        );
        assert_eq!(
            valid.report.status,
            Completeness::Complete,
            "{}",
            valid.render_human()
        );

        let invalid = check_source(
            r#"
                artifact A {}
                fn identity[T](value: T) -> (result: T)
                where T: Copyable {
                    result = value
                }
                process P {
                    gets input: A
                    () => {
                        let missing = identity()
                        let owned = identity(value: input)
                    }
                }
            "#,
        );
        assert_eq!(invalid.report.status, Completeness::Invalid);
        for code in ["E0827", "E0828"] {
            assert!(
                invalid
                    .report
                    .diagnostics
                    .iter()
                    .any(|diagnostic| diagnostic.code == code),
                "missing diagnostic {code}"
            );
        }
    }

    #[test]
    fn validates_exact_trait_implementation_signatures() {
        let valid = check_source(
            r#"
                trait Validatable[T] {
                    fn valid(value: T) -> (result: Bool)
                }
                impl Validatable[Integer] {
                    fn valid(value: Integer) -> (result: Bool) {
                        result = true
                    }
                }
            "#,
        );
        assert_eq!(
            valid.report.status,
            Completeness::Complete,
            "{}",
            valid.render_human()
        );

        let invalid = check_source(
            r#"
                trait Validatable[T] {
                    fn valid(value: T) -> (result: Bool)
                }
                impl Validatable[Integer] {
                    fn valid(value: Scalar) -> (result: Integer) {
                        result = 1
                    }
                }
                impl Validatable[Scalar] {
                    fn nope(value: Scalar) -> (result: Bool) {
                        result = true
                    }
                }
            "#,
        );
        assert_eq!(invalid.report.status, Completeness::Invalid);
        for code in ["E0424", "E0422", "E0425"] {
            assert!(
                invalid
                    .report
                    .diagnostics
                    .iter()
                    .any(|diagnostic| diagnostic.code == code),
                "missing diagnostic {code}"
            );
        }
    }

    #[test]
    fn records_consumed_inputs_and_yield_provenance() {
        let compilation = check_source(
            r#"
                artifact Input {}
                artifact Produced {}
                artifact Container { item: Input }

                process Consumes {
                    gets input: Input
                    () => {}
                }
                process Exports {
                    gets input: Input
                    export output: Input
                    () => { output = input }
                }
                process Contains {
                    gets input: Input
                    export output: Container
                    () => { output = Container(item: input) }
                }
                process Streams {
                    gets input: Input
                    export outputs: Stream[Produced]
                    () => { yield outputs = Produced() }
                }
                process Alias {
                    gets input: Input
                    export output: Input
                    () => {
                        let local = input
                        output = local
                    }
                }
                process AggregateAlias {
                    gets input: Input
                    export output: Container
                    () => {
                        let local = Container(item: input)
                        output = local
                    }
                }
                process Branches {
                    gets input: Input
                    export output: Container
                    (flag: Bool) => {
                        if flag {
                            output = Container(item: input)
                        } else {
                            output = Container(item: input)
                        }
                    }
                }
                process Appends {
                    gets input: Input
                    export output: Container
                    () => {
                        let mut items: Input[] = []
                        items.append(input)
                        output = Container(item: items[0])
                    }
                }
                process Iterates {
                    gets inputs: Input[]
                    export outputs: Stream[Input]
                    () => {
                        for input in inputs {
                            yield outputs = input
                        }
                    }
                }
                process LocalProduced {
                    export output: Produced
                    () => {
                        let local = Produced()
                        output = local
                    }
                }
            "#,
        );
        assert_eq!(compilation.report.status, Completeness::Complete);
        let consumed = compilation
            .report
            .provenance
            .iter()
            .filter(|record| record.kind == "consumed_by")
            .map(|record| record.artifact.as_str())
            .collect::<Vec<_>>();
        assert!(
            consumed
                .iter()
                .any(|artifact| artifact.starts_with("Consumes.input:"))
        );
        assert!(
            consumed
                .iter()
                .any(|artifact| artifact.starts_with("Streams.input:"))
        );
        assert!(
            !consumed
                .iter()
                .any(|artifact| artifact.starts_with("Exports.input:"))
        );
        assert!(
            !consumed
                .iter()
                .any(|artifact| artifact.starts_with("Contains.input:"))
        );
        for process in ["Alias", "AggregateAlias", "Branches", "Appends", "Iterates"] {
            assert!(
                !consumed
                    .iter()
                    .any(|artifact| artifact.starts_with(&format!("{process}.input"))),
                "{process} input was incorrectly classified as consumed"
            );
        }
        assert!(compilation.report.provenance.iter().any(|record| {
            record.kind == "produced" && record.artifact.starts_with("Streams.outputs:Produced")
        }));
        assert!(compilation.report.provenance.iter().any(|record| {
            record.kind == "produced"
                && record.artifact.starts_with("LocalProduced.output:Produced")
        }));
    }

    #[test]
    fn constrains_read_only_loans_and_orders_mutating_borrowers() {
        let read_only = check_source(
            r#"
                artifact Tool { state: Integer }
                process Observe {
                    uses tool: Tool
                    () => { requires tool.state >= 0 }
                }
                process ReadTwice {
                    gets tool: Tool
                    () => {
                        let observer = Observe()
                        let first = observer()
                        let second = observer()
                        tool -> first.tool
                        tool -> second.tool
                    }
                }
                process Wrapper {
                    gets tool: Tool
                    () => {
                        let configured = ReadTwice()
                        let batch = configured()
                        tool -> batch.tool
                    }
                }
            "#,
        );
        assert_eq!(read_only.report.status, Completeness::Complete);
        assert!(read_only.report.constraints.iter().any(|constraint| {
            constraint.phase == "scheduling"
                && constraint.path.starts_with("Wrapper.batch.scheduling.loan")
                && constraint.predicate.contains("non_overlap(first, second")
        }));

        let mutating = check_source(
            r#"
                artifact Tool { state: Integer }
                process Observe { uses tool: Tool () => { requires tool.state >= 0 } }
                process Mutate { uses tool: Tool () => { tool.state = 1 } }
                process Conflict {
                    gets tool: Tool
                    () => {
                        let observer = Observe()
                        let mutator = Mutate()
                        let read = observer()
                        let write = mutator()
                        tool -> read.tool
                        tool -> write.tool
                    }
                }
            "#,
        );
        assert_eq!(mutating.report.status, Completeness::Invalid);
        assert!(
            mutating
                .report
                .diagnostics
                .iter()
                .any(|diagnostic| diagnostic.code == "E0714")
        );

        let ordered = check_source(
            r#"
                artifact Tool { state: Integer }
                process Observe { uses tool: Tool () => { requires tool.state >= 0 } }
                process Mutate { uses tool: Tool () => { tool.state = 1 } }
                process Ordered {
                    gets tool: Tool
                    () => {
                        let observer = Observe()
                        let mutator = Mutate()
                        let read = observer()
                        let write = mutator()
                        tool -> read.tool
                        tool -> write.tool
                        write before read
                    }
                }
            "#,
        );
        assert_eq!(
            ordered.report.status,
            Completeness::Complete,
            "{}",
            ordered.render_human()
        );

        let cyclic = check_source(
            r#"
                process Child { () => {} }
                process Cyclic {
                    () => {
                        let child = Child()
                        let first = child()
                        let second = child()
                        first before second
                        second before first
                    }
                }
            "#,
        );
        assert_eq!(cyclic.report.status, Completeness::Invalid);
        assert!(
            cyclic
                .report
                .diagnostics
                .iter()
                .any(|diagnostic| diagnostic.code == "E0713")
        );
    }

    #[test]
    fn rejects_invalid_bounds_trait_bodies_and_maybe_duplicate_results() {
        let compilation = check_source(
            r#"
                artifact A {}
                fn bad_bound[T](value: T) -> (result: Bool)
                where T: MadeUp, U: Copyable {
                    result = true
                }
                fn add[T](left: T, right: T) -> (result: T)
                where T: Add, T: Copyable {
                    result = left + right
                }
                fn maybe_twice(flag: Bool) -> (result: Integer) {
                    if flag { result = 1 }
                    result = 2
                }
                fn duplicates(value: Integer, value: Integer) -> (
                    result: Integer,
                    result: Integer
                ) { result = value }
                trait Check[T] {
                    fn check(value: T) -> (result: Bool) { result = true }
                }
                trait Identity[T] {
                    fn identity(value: T) -> (result: Bool)
                }
                impl Identity[Missing] {
                    fn identity(value: Missing) -> (result: Bool) { result = true }
                }
            "#,
        );
        assert_eq!(compilation.report.status, Completeness::Invalid);
        for code in ["E0432", "E0431", "E0414", "E0428", "E0429", "E0435"] {
            assert!(
                compilation
                    .report
                    .diagnostics
                    .iter()
                    .any(|diagnostic| diagnostic.code == code),
                "missing diagnostic {code}: {}",
                compilation.render_human()
            );
        }
        assert!(
            !compilation
                .report
                .diagnostics
                .iter()
                .any(|diagnostic| diagnostic.code == "E0809"),
            "generic Add body should type-check from its explicit capability"
        );
    }

    #[test]
    fn trait_impl_coherence_uses_structural_unit_types_and_copyable_results() {
        let compilation = check_source(
            r#"
                artifact A {}
                trait Check[T] { fn check(value: T) -> (result: Bool) }
                impl Check[Length] {
                    fn check(value: Length) -> (result: Bool) { result = true }
                }
                impl Check[Meter] {
                    fn check(value: Meter) -> (result: Bool) { result = true }
                }
                trait Identity[T] {
                    fn identity(value: T) -> (result: T) where T: Copyable
                }
                impl Identity[A] {
                    fn identity(value: A) -> (result: A) { result = value }
                }
            "#,
        );
        assert_eq!(compilation.report.status, Completeness::Invalid);
        assert!(
            compilation
                .report
                .diagnostics
                .iter()
                .any(|diagnostic| diagnostic.code == "E0404")
        );
        assert!(
            compilation
                .report
                .diagnostics
                .iter()
                .any(|diagnostic| diagnostic.code == "E0402")
        );
    }

    #[test]
    fn generic_calls_cannot_invent_bounds_and_false_pure_requirements_fail() {
        let compilation = check_source(
            r#"
                artifact A {}
                fn identity[U](value: U) -> (result: U)
                where U: Copyable {
                    result = value
                }
                fn wrapper[T](value: T) -> (result: Bool) {
                    let copied = identity(value: value)
                    result = true
                }
                fn impossible() -> (result: Bool) {
                    requires false
                    result = true
                }
            "#,
        );
        assert_eq!(compilation.report.status, Completeness::Invalid);
        for code in ["E0828", "E0436"] {
            assert!(
                compilation
                    .report
                    .diagnostics
                    .iter()
                    .any(|diagnostic| diagnostic.code == code),
                "missing diagnostic {code}: {}",
                compilation.render_human()
            );
        }
    }

    #[test]
    fn rejects_copyable_ownership_ports_and_use_after_move() {
        let compilation = check_source(
            r#"
                artifact Item { mass: Mass }
                artifact Container { item: Item }
                process InvalidPorts {
                    gets count: Integer
                    uses duration: Time
                    () => {}
                }
                process ReusesArtifact {
                    gets input: Item
                    export output: Item
                    () => {
                        output = input
                        requires input.mass > 0 Kilogram
                    }
                }
                process ReusesStream {
                    gets input: Stream[Integer]
                    export output: Stream[Integer]
                    () => {
                        for item in input {
                            requires item >= 0
                        }
                        output = input
                    }
                }
                process InsertsIntoArray {
                    gets input: Item
                    export output: Item
                    () => {
                        let mut local: Item[] = []
                        local = [input]
                        output = input
                    }
                }
                process ReusesIndexedChild {
                    gets inputs: Item[]
                    export first: Item
                    export second: Item
                    () => {
                        first = inputs[0]
                        second = inputs[0]
                    }
                }
                process ReusesMemberChild {
                    gets container: Container
                    export first: Item
                    export second: Item
                    () => {
                        first = container.item
                        second = container.item
                    }
                }
                process ExportsBorrowed {
                    uses input: Item
                    export output: Item
                    () => { output = input }
                }
                process MutatesAfterMove {
                    gets input: Item
                    export output: Item
                    () => {
                        output = input
                        input.mass = 1 Kilogram
                    }
                }
            "#,
        );
        assert_eq!(compilation.report.status, Completeness::Invalid);
        for code in ["E0506", "E0507", "E0618", "E0702", "E0704"] {
            assert!(
                compilation
                    .report
                    .diagnostics
                    .iter()
                    .any(|diagnostic| diagnostic.code == code),
                "missing diagnostic {code}: {}",
                compilation.render_human()
            );
        }
    }

    #[test]
    fn trait_bounds_use_exact_structural_types_and_impl_signatures_are_ordered() {
        let compilation = check_source(
            r#"
                trait Mark[T] { fn marked(value: T) -> (result: Bool) }
                impl Mark[Integer] {
                    fn marked(value: Integer) -> (result: Bool) { result = true }
                }
                fn needs_mark[U](value: U) -> (result: Bool)
                where U: Mark {
                    result = true
                }
                fn wrong_bound() -> (result: Bool) {
                    let checked = needs_mark(value: 1.5)
                    result = checked.result
                }

                trait Pair[T] {
                    fn pair(first: T, second: Bool) -> (result: Bool)
                }
                impl Pair[Integer] {
                    fn pair(second: Bool, first: Integer) -> (result: Bool) {
                        result = second
                    }
                }
            "#,
        );
        assert_eq!(compilation.report.status, Completeness::Invalid);
        for code in ["E0828", "E0424"] {
            assert!(
                compilation
                    .report
                    .diagnostics
                    .iter()
                    .any(|diagnostic| diagnostic.code == code),
                "missing diagnostic {code}: {}",
                compilation.render_human()
            );
        }
    }

    #[test]
    fn supports_bounded_symbolic_integer_ranges() {
        let valid = check_source(
            r#"
                import math
                artifact Item {}
                process MakeItems {
                    export items: Stream[Item]
                    (count: Integer) => {
                        requires count >= 0
                        for index in math.range(0, count) {
                            requires index >= 0
                            yield items = Item()
                        }
                    }
                }
            "#,
        );
        assert_eq!(valid.report.status, Completeness::Complete);

        let invalid = check_source(
            r#"
                import math
                process BadRange {
                    () => {
                        for value in math.range(0, 1.5) {
                            requires value >= 0
                        }
                    }
                }
            "#,
        );
        assert_eq!(invalid.report.status, Completeness::Invalid);
        assert!(
            invalid
                .report
                .diagnostics
                .iter()
                .any(|diagnostic| diagnostic.code == "E0829")
        );
    }

    #[test]
    fn range_loops_enforce_cardinality_effects_and_quantify_guards() {
        let invalid = check_source(
            r#"
                import math
                artifact Item {}
                process Child { () => {} }
                process InvalidLoops {
                    gets captured: Item
                    export value: Integer
                    export items: Stream[Item]
                    () => {
                        let mut count: Integer = 0
                        let child = Child()
                        for index in math.range(0, 2) {
                            value = index
                            yield items = captured
                            count = count + 1
                            let run = child()
                        }
                    }
                }
                process InvalidGuard {
                    () => {
                        for index in math.range(-1, 0) {
                            requires index >= 0
                        }
                    }
                }
                artifact Tool { state: Integer }
                process InvalidMutation {
                    uses tool: Tool
                    (end: Integer) => {
                        for index in math.range(0, end) {
                            tool.state = index
                        }
                    }
                }
            "#,
        );
        assert_eq!(invalid.report.status, Completeness::Invalid);
        for code in ["E0630", "E0631", "E0633", "E0703", "E0603"] {
            assert!(
                invalid
                    .report
                    .diagnostics
                    .iter()
                    .any(|diagnostic| diagnostic.code == code),
                "missing diagnostic {code}: {}",
                invalid.render_human()
            );
        }

        let empty = check_source(
            r#"
                import math
                process EmptyRange {
                    () => {
                        for index in math.range(0, 0) {
                            requires false
                        }
                    }
                }
            "#,
        );
        assert_eq!(
            empty.report.status,
            Completeness::Complete,
            "{}",
            empty.render_human()
        );

        let wide_false = check_source(
            r#"
                import math
                process WideFalseRange {
                    () => {
                        for index in math.range(0, 1025) {
                            requires false
                        }
                    }
                }
            "#,
        );
        assert!(
            wide_false
                .report
                .diagnostics
                .iter()
                .any(|diagnostic| diagnostic.code == "E0603"),
            "{}",
            wide_false.render_human()
        );

        let quantified = check_source(
            r#"
                import math
                process SymbolicRange {
                    (end: Integer) => {
                        for index in math.range(0, end) {
                            requires index >= 0
                        }
                    }
                }
            "#,
        );
        let constraint = quantified
            .report
            .constraints
            .iter()
            .find(|constraint| constraint.predicate.contains("math.range"))
            .expect("range guard should be quantified");
        assert!(constraint.predicate.starts_with("every(math.range"));
        assert!(!constraint.dependencies.iter().any(|name| name == "index"));
        assert!(!constraint.dependencies.iter().any(|name| name == "every"));
        assert!(constraint.dependencies.iter().any(|name| name == "end"));
    }

    #[test]
    fn supports_stream_batching_with_conditional_yields_and_reset() {
        let compilation = check_source(
            r#"
                artifact Item {}
                artifact Batch { items: Item[] }
                process BatchItems {
                    gets input: Stream[Item]
                    export batches: Stream[Batch]
                    (batch_size: Integer) => {
                        requires batch_size > 0
                        let mut collected: Item[] = []
                        for item in input {
                            collected.append(item)
                            if collected.length() == batch_size {
                                yield batches = Batch(items: collected)
                                collected = []
                            }
                        }
                    }
                }
            "#,
        );
        assert_eq!(
            compilation.report.status,
            Completeness::Complete,
            "{}",
            compilation.render_human()
        );
        assert!(compilation.report.obligations.is_empty());

        let invalid = check_source(
            r#"
                artifact Item {}
                process ImmutableAccumulator {
                    gets input: Item
                    () => {
                        const collected: Item[] = []
                        collected.append(input)
                        collected = []
                    }
                }
            "#,
        );
        for code in ["E0626", "E0625"] {
            assert!(
                invalid
                    .report
                    .diagnostics
                    .iter()
                    .any(|diagnostic| diagnostic.code == code),
                "missing diagnostic {code}: {}",
                invalid.render_human()
            );
        }

        let named_range = check_source(
            r#"
                import math
                process NamedRange {
                    () => {
                        for item in math.range(start: 0, end: 1) {
                            requires item >= 0
                        }
                    }
                }
            "#,
        );
        assert!(
            named_range
                .report
                .diagnostics
                .iter()
                .any(|diagnostic| diagnostic.code == "E0830")
        );
    }

    #[test]
    fn graph_wires_trace_appended_arrays_back_to_their_run_export() {
        let compilation = check_source(
            r#"
                artifact Item {}

                process Produce {
                    gets item: Item
                    export items: Stream[Item]
                    () => { yield items = item }
                }

                process Consume {
                    gets items: Item[]
                    () => {}
                }

                process Factory {
                    gets item: Item
                    () => {
                        let produce = Produce()()
                        item -> produce.item

                        let mut collected: Item[] = []
                        for produced in produce.items {
                            collected.append(produced)
                        }

                        let consume = Consume()()
                        collected -> consume.items
                    }
                }
            "#,
        );
        assert_eq!(
            compilation.report.status,
            Completeness::Complete,
            "{}",
            compilation.render_human()
        );
        let wire = compilation.model.processes["Factory"]
            .wires
            .iter()
            .find(|wire| wire.target == "consume.items")
            .expect("consumer wire");
        assert_eq!(wire.source, "produce.items");
    }

    #[test]
    fn zip_preserves_owned_pairs_and_allows_sequential_tail_consumption() {
        let compilation = check_source(
            r#"
                import std.stream
                artifact Left {}
                artifact Right {}
                artifact Pair {
                    left: Left
                    right: Right
                }
                process PairStreams {
                    gets lefts: Stream[Left]
                    gets rights: Stream[Right]
                    export pairs: Stream[Pair]
                    export remaining_lefts: Stream[Left]
                    () => {
                        for pair in std.stream.zip(
                            left: lefts,
                            right: rights
                        ) {
                            yield pairs = Pair(
                                left: pair.left,
                                right: pair.right
                            )
                        }
                        for left in lefts {
                            yield remaining_lefts = left
                        }
                    }
                }
            "#,
        );
        assert_eq!(
            compilation.report.status,
            Completeness::Complete,
            "{}",
            compilation.render_human()
        );
        assert!(compilation.report.obligations.is_empty());

        let exact_reuse = check_source(
            r#"
                import std.stream
                artifact Item {}
                process ExactThenReuse {
                    gets lefts: Stream[Item]
                    gets rights: Stream[Item]
                    export remaining: Stream[Item]
                    () => {
                        for pair in std.stream.zip_exact(lefts, rights) {
                            requires true
                        }
                        for left in lefts {
                            yield remaining = left
                        }
                    }
                }
            "#,
        );
        assert!(
            exact_reuse
                .report
                .diagnostics
                .iter()
                .any(|diagnostic| diagnostic.code == "E0701")
        );

        let invalid = check_source(
            r#"
                import std.stream
                process P {
                    gets values: Stream[Integer]
                    () => {
                        for pair in std.stream.zip(values, 1) {
                            requires pair.left > 0
                        }
                    }
                }
            "#,
        );
        assert!(
            invalid
                .report
                .diagnostics
                .iter()
                .any(|diagnostic| diagnostic.code == "E0831")
        );
    }

    #[test]
    fn constraints_use_full_instance_paths_and_elide_satisfied_predicates() {
        let compilation = check_source(
            r#"
                unit Millimeter = Meter / 1000
                artifact Block { thickness: Length }
                process Cut {
                    gets block: Block
                    config thickness: Length
                    () => {
                        requires thickness > 0 Meter
                        requires thickness <= block.thickness
                    }
                }
                process Factory {
                    gets block: Block
                    config thickness: Length
                    () => {
                        let cutter = Cut(thickness: thickness)
                        let cut = cutter()
                        block -> cut.block
                    }
                }
                process Example {
                    gets block: Block
                    () => {
                        let factory = Factory(thickness: 5 Millimeter)
                        let batch = factory()
                        block -> batch.block
                    }
                }
            "#,
        );
        assert_eq!(
            compilation.report.status,
            Completeness::Complete,
            "{}",
            compilation.render_human()
        );
        assert_eq!(compilation.report.constraints.len(), 1);
        let constraint = &compilation.report.constraints[0];
        assert_eq!(constraint.path, "Example.batch.cut.requirement[1]");
        assert_eq!(constraint.predicate, "(5 Millimeter <= block.thickness)");
        assert_eq!(constraint.dependencies, ["block"]);
        assert!(compilation.render_human().contains(
            "[invocation] Example.batch.cut.requirement[1]: (5 Millimeter <= block.thickness)"
        ));

        let contradiction = check_source(
            r#"
                unit Millimeter = Meter / 1000
                process Limit {
                    config thickness: Length
                    () => { requires thickness <= 100 Millimeter }
                }
                process Example {
                    () => {
                        let limit = Limit(thickness: 5 Meter)
                        let run = limit()
                    }
                }
            "#,
        );
        assert_eq!(contradiction.report.status, Completeness::Invalid);
        let diagnostic = contradiction
            .report
            .diagnostics
            .iter()
            .find(|diagnostic| diagnostic.code == "E0901")
            .expect("configured false constraint should be a contradiction");
        assert!(diagnostic.message.contains("Example.run.requirement[0]"));
        assert!(contradiction.report.constraints.is_empty());
    }

    #[test]
    fn stream_guards_do_not_select_one_of_multiple_yield_sites() {
        for yields in [
            "yield items = Item(value: -1)\nyield items = Item(value: 1)",
            "yield items = Item(value: 1)\nyield items = Item(value: -1)",
        ] {
            let compilation = check_source(&format!(
                r#"
                    artifact Item {{ value: Integer }}
                    process Producer {{
                        export items: Stream[Item]
                        () => {{ {yields} }}
                    }}
                    process Consumer {{
                        gets items: Stream[Item]
                        () => {{
                            for item in items {{
                                requires item.value > 0
                            }}
                        }}
                    }}
                    process Example {{
                        () => {{
                            let producer = Producer()
                            let produced = producer()
                            let consumer = Consumer()
                            let consumed = consumer()
                            produced.items -> consumed.items
                        }}
                    }}
                "#
            ));
            assert_eq!(
                compilation.report.status,
                Completeness::Complete,
                "{}",
                compilation.render_human()
            );
            assert_eq!(compilation.report.constraints.len(), 1);
            assert_eq!(
                compilation.report.constraints[0].path,
                "Example.consumed.requirement[0]"
            );
            assert!(
                compilation.report.constraints[0]
                    .predicate
                    .contains("item.value")
            );
        }

        let conditional = check_source(
            r#"
                artifact Item { value: Integer }
                process Producer {
                    export items: Stream[Item]
                    (flag: Bool) => {
                        if flag {
                            yield items = Item(value: -1)
                        } else {
                            yield items = Item(value: 1)
                        }
                    }
                }
                process Consumer {
                    gets items: Stream[Item]
                    () => {
                        for item in items { requires item.value > 0 }
                    }
                }
                process Example {
                    (flag: Bool) => {
                        let producer = Producer()
                        let produced = producer(flag: flag)
                        let consumer = Consumer()
                        let consumed = consumer()
                        produced.items -> consumed.items
                    }
                }
            "#,
        );
        assert_eq!(conditional.report.status, Completeness::Complete);
        assert_eq!(conditional.report.constraints.len(), 1);
        assert!(
            conditional.report.constraints[0]
                .predicate
                .contains("item.value")
        );
    }

    #[test]
    fn stream_substitution_reaches_nested_guard_lambdas_without_capturing_locals() {
        let source = r#"
                import math
                artifact Item { value: Integer }
                process Producer {
                    export items: Stream[Item]
                    () => { yield items = Item(value: 2) }
                }
                process Consumer {
                    gets items: Stream[Item]
                    () => {
                        for item in items {
                            for index in math.range(0, 2) {
                                requires item.value > index
                            }
                        }
                    }
                }
                process Example {
                    () => {
                        let producer = Producer()
                        let produced = producer()
                        let consumer = Consumer()
                        let consumed = consumer()
                        produced.items -> consumed.items
                    }
                }
            "#;
        let compilation = check_source(source);
        assert_eq!(compilation.report.status, Completeness::Complete);
        assert!(
            compilation.report.constraints.is_empty(),
            "the concrete predicate 2 > index holds for index in [0, 2)"
        );
        let symbolic = check_source(
            &source
                .replace(
                    "gets items: Stream[Item]",
                    "config limit: Integer\ngets items: Stream[Item]",
                )
                .replace("math.range(0, 2)", "math.range(0, limit)"),
        );
        assert!(
            symbolic.report.diagnostics.is_empty(),
            "{}",
            symbolic.render_human()
        );
        assert_eq!(symbolic.report.constraints.len(), 1);
        let predicate = &symbolic.report.constraints[0].predicate;
        assert!(predicate.contains("2 > index"), "{predicate}");
        assert!(!predicate.contains("item.value"), "{predicate}");
    }

    #[test]
    fn stream_substitutions_are_scoped_to_their_requirement() {
        let compilation = check_source(
            r#"
                artifact Item { value: Integer }
                process Producer {
                    export items: Stream[Item]
                    (value: Integer) => { yield items = Item(value: value) }
                }
                process Consumer {
                    gets negative: Stream[Item]
                    gets positive: Stream[Item]
                    () => {
                        for item in negative { requires item.value < 0 }
                        for item in positive { requires item.value > 0 }
                    }
                }
                process Example {
                    () => {
                        let producer = Producer()
                        let negative = producer(value: -1)
                        let positive = producer(value: 1)
                        let consumer = Consumer()
                        let consumed = consumer()
                        negative.items -> consumed.negative
                        positive.items -> consumed.positive
                    }
                }
            "#,
        );
        assert_eq!(
            compilation.report.status,
            Completeness::Complete,
            "{}",
            compilation.render_human()
        );
        assert!(compilation.report.constraints.is_empty());
    }

    #[test]
    fn local_names_cannot_shadow_process_or_lexical_names() {
        let compilation = check_source(
            r#"
                artifact Item { value: Integer }
                process Invalid {
                    gets items: Stream[Item]
                    config threshold: Integer
                    (items: Integer, count: Integer) => {
                        let threshold = 1
                        for count in [1, 2] { requires count > 0 }
                    }
                }
            "#,
        );
        assert_eq!(compilation.report.status, Completeness::Invalid);
        assert!(
            compilation
                .report
                .diagnostics
                .iter()
                .filter(|diagnostic| diagnostic.code == "E0634")
                .count()
                >= 3,
            "{}",
            compilation.render_human()
        );
    }

    #[test]
    fn new_process_locals_require_let_without_hiding_later_semantics() {
        let compilation = check_source(
            r#"
                process Child { (count: Integer) => { requires count > 0 } }
                process Parent {
                    () => {
                        child = Child()
                        run = child(count: 1)
                    }
                }
            "#,
        );
        assert_eq!(compilation.report.status, Completeness::Invalid);
        assert_eq!(
            compilation
                .report
                .diagnostics
                .iter()
                .filter(|diagnostic| diagnostic.code == "E0635")
                .count(),
            2,
            "{}",
            compilation.render_human()
        );
        assert!(
            !compilation
                .report
                .diagnostics
                .iter()
                .any(|diagnostic| diagnostic.code == "E0813"),
            "{}",
            compilation.render_human()
        );
        assert!(
            compilation.model.processes["Parent"]
                .runs
                .contains_key("run")
        );

        let mutable_graph_binding = check_source(
            r#"
                process Child {}
                process Parent {
                    () => { let child = Child()
child = Child() }
                }
            "#,
        );
        assert!(
            mutable_graph_binding
                .report
                .diagnostics
                .iter()
                .any(|diagnostic| diagnostic.code == "E0625"),
            "{}",
            mutable_graph_binding.render_human()
        );
    }

    #[test]
    fn rejects_recursive_process_instantiation() {
        let compilation = check_source(
            r#"
                process Recursive {
                    () => {
                        let configured = Recursive()
                        let run = configured()
                    }
                }
            "#,
        );
        assert_eq!(compilation.report.status, Completeness::Invalid);
        assert!(
            compilation
                .report
                .diagnostics
                .iter()
                .any(|diagnostic| diagnostic.code == "E0715")
        );
    }

    #[test]
    fn unused_imported_processes_are_not_constraint_instances() {
        let directory = tempdir().unwrap();
        let library = directory.path().join("library.req");
        let main = directory.path().join("main.req");
        fs::write(
            &library,
            r#"
                process Used {
                    (value: Integer) => { requires value > 0 }
                }
                process Unused {
                    (unresolved: Integer) => { requires unresolved > 0 }
                }
            "#,
        )
        .unwrap();
        fs::write(
            &main,
            r#"
                import "library.req" (Used, Unused)
                process Example {
                    () => {
                        let configured = Used()
                        let run = configured(value: 1)
                    }
                }
            "#,
        )
        .unwrap();
        let compilation = Compiler::new(CompilerOptions::default())
            .check_path(&main)
            .unwrap();
        assert_eq!(
            compilation.report.status,
            Completeness::Complete,
            "{}",
            compilation.render_human()
        );
        assert!(compilation.report.constraints.is_empty());
    }

    #[test]
    fn configured_process_fields_accept_conforming_implementations() {
        let compilation = check_source(
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

                    () => {
                        let selected = ConcreteStrategy(limit: 2)
                        let factory = Factory(strategy: selected)
                        let batch = factory()
                        feed -> batch.feed
                        batch.product -> product
                    }
                }
            "#,
        );
        assert_eq!(
            compilation.report.status,
            Completeness::Complete,
            "{}",
            compilation.render_human()
        );
        assert!(compilation.report.constraints.is_empty());
        let factory = &compilation.model.processes["Factory"];
        assert_eq!(
            factory.members["strategy"].ty,
            Type::ConfiguredProcess("Strategy".into())
        );
        assert_eq!(
            factory.runs["execute"].process_source.as_deref(),
            Some("strategy")
        );
        let example_run = &compilation.model.processes["Example"].runs["batch"];
        assert_eq!(
            example_run.configured_arguments["strategy"].process,
            "ConcreteStrategy"
        );
    }

    #[test]
    fn configures_and_invokes_a_process_in_one_chained_expression() {
        let compilation = check_source(
            r#"
                process Cut {
                    config thickness: Length
                    (count: Integer) => {
                        requires thickness > 0 Meter
                        requires count > 0
                    }
                }
                process Factory {
                    () => {
                        let cut = Cut(thickness: 2 Meter)(count: 3)
                    }
                }
            "#,
        );
        assert_eq!(
            compilation.report.status,
            Completeness::Complete,
            "{}",
            compilation.render_human()
        );
        let run = &compilation.model.processes["Factory"].runs["cut"];
        assert_eq!(run.process, "Cut");
        assert_eq!(run.config["thickness"], "2 Meter");
        assert_eq!(run.arguments["count"], "3");
        assert!(compilation.report.constraints.is_empty());
    }

    #[test]
    fn configured_process_fields_reject_structural_nonimplementations() {
        let compilation = check_source(
            r#"
                process Strategy {}
                process Lookalike {}
                process Factory { config strategy: Configured[Strategy] }
                process Example {
                    () => {
                        let selected = Lookalike()
                        let factory = Factory(strategy: selected)
                    }
                }
            "#,
        );
        assert_eq!(compilation.report.status, Completeness::Invalid);
        assert!(compilation.report.diagnostics.iter().any(|diagnostic| {
            diagnostic.code == "E0621"
                && diagnostic.message.contains("Configured[Strategy]")
                && diagnostic.message.contains("Configured[Lookalike]")
        }));
    }

    #[test]
    fn logical_operators_type_check_and_constant_fold() {
        let complete = check_source(
            r#"
                process Root {
                    export cost: USD aggregate sum
                    (width: Length) => {
                        requires width > 0 Meter and width < 10 Meter
                        requires width > 0 Meter or width == 0 Meter
                        requires not (width == 5 Meter)
                        cost = 1 USD
                    }
                }
            "#,
        );
        assert_eq!(
            complete.report.status,
            Completeness::Complete,
            "{}",
            complete.render_human()
        );
        assert_eq!(complete.report.constraints.len(), 3);
        assert!(
            complete.report.constraints[0].predicate.contains("and"),
            "the residual predicate keeps the logical operator: {}",
            complete.report.constraints[0].predicate
        );

        // The dead `&&`/`||` branch in `evaluate_predicate` is now reachable.
        let folded = check_source(
            r#"
                process Root {
                    export cost: USD aggregate sum
                    () => {
                        requires 1 Meter > 0 Meter and 2 Meter > 3 Meter
                        cost = 1 USD
                    }
                }
            "#,
        );
        assert_eq!(folded.report.status, Completeness::Invalid);
        assert!(
            folded
                .report
                .diagnostics
                .iter()
                .any(|diagnostic| diagnostic.code == "E0603"),
            "{}",
            folded.render_human()
        );

        let discharged = check_source(
            r#"
                process Root {
                    export cost: USD aggregate sum
                    () => {
                        requires 1 Meter > 0 Meter or 2 Meter > 3 Meter
                        cost = 1 USD
                    }
                }
            "#,
        );
        assert_eq!(
            discharged.report.status,
            Completeness::Complete,
            "{}",
            discharged.render_human()
        );
        assert!(
            discharged.report.constraints.is_empty(),
            "a statically true disjunction discharges"
        );
    }

    #[test]
    fn logical_operators_reject_non_boolean_operands() {
        let compilation = check_source(
            r#"
                process Root {
                    export cost: USD aggregate sum
                    () => {
                        requires 1 Meter and true
                        cost = 1 USD
                    }
                }
            "#,
        );
        assert_eq!(compilation.report.status, Completeness::Invalid);
        assert!(
            compilation
                .report
                .diagnostics
                .iter()
                .any(|diagnostic| diagnostic.code == "E0835"),
            "{}",
            compilation.render_human()
        );
    }

    /// A string literal used to infer as `Type::Unknown`, which `compatible()`
    /// accepted against everything, so this compiled clean.
    #[test]
    fn string_literals_cannot_satisfy_a_dimensional_annotation() {
        let compilation = check_source(
            r#"
                process Root {
                    export cost: USD aggregate sum
                    () => {
                        let bogus: Length = "not a length"
                        requires bogus > 0 Meter
                        cost = 1 USD
                    }
                }
            "#,
        );
        assert_eq!(
            compilation.report.status,
            Completeness::Invalid,
            "{}",
            compilation.render_human()
        );
        assert!(
            compilation.report.diagnostics.iter().any(|diagnostic| {
                diagnostic.code == "E0601" && diagnostic.message.contains("found String")
            }),
            "{}",
            compilation.render_human()
        );
    }

    #[test]
    fn string_literals_are_usable_where_text_makes_sense() {
        let compilation = check_source(
            r#"
                process Root {
                    config label: String = "hello\nworld"
                    export cost: USD aggregate sum
                    () => {
                        requires label == "hello\nworld"
                        cost = 1 USD
                    }
                }
            "#,
        );
        assert_eq!(
            compilation.report.status,
            Completeness::Complete,
            "{}",
            compilation.render_human()
        );
        assert!(
            compilation.report.constraints.is_empty(),
            "equal text literals fold away"
        );
        assert_eq!(
            compilation.model.processes["Root"].members["label"].ty,
            Type::String
        );

        let mismatched = check_source(
            r#"
                process Root {
                    export cost: USD aggregate sum
                    () => {
                        requires "abc" > 0 Meter
                        cost = 1 USD
                    }
                }
            "#,
        );
        assert_eq!(mismatched.report.status, Completeness::Invalid);
        assert!(
            mismatched.report.diagnostics.iter().any(|diagnostic| {
                diagnostic.code == "E0808" && diagnostic.message.contains("String")
            }),
            "{}",
            mismatched.render_human()
        );
    }
}
