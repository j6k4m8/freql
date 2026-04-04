use serde::Serialize;

use crate::diagnostic::{Diagnostic, Severity, SourceSpan};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Completeness {
    Complete,
    Partial,
    Invalid,
}

#[derive(Clone, Debug, Serialize)]
pub struct SymbolReport {
    pub path: String,
    pub kind: String,
    pub span: SourceSpan,
}

#[derive(Clone, Debug, Serialize)]
pub struct Obligation {
    pub id: String,
    pub kind: String,
    pub path: String,
    pub expected: String,
    pub message: String,
    pub span: SourceSpan,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub dependencies: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub condition: Option<String>,
}

#[derive(Clone, Debug, Serialize)]
pub struct Constraint {
    pub id: String,
    pub path: String,
    pub phase: String,
    pub predicate: String,
    pub status: String,
    pub span: SourceSpan,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub dependencies: Vec<String>,
}

#[derive(Clone, Debug, Serialize)]
pub struct ProvenanceRecord {
    pub id: String,
    pub kind: String,
    pub artifact: String,
    pub run: String,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub sources: Vec<String>,
    pub span: SourceSpan,
}

#[derive(Clone, Debug, Serialize)]
pub struct AssumedZero {
    pub path: String,
    pub contract: String,
    pub reason: String,
    pub span: SourceSpan,
}

#[derive(Clone, Debug, Serialize)]
pub struct MetricBounds {
    /// Exact lower bound rendered in one consistent author-facing unit.
    pub lower: String,
    /// Exact upper bound rendered in the same unit as `lower`.
    pub upper: String,
    /// Why the metric remains a bounded range instead of one exact value.
    pub reason: String,
}

#[derive(Clone, Debug, Serialize)]
pub struct MonitoredMetric {
    pub path: String,
    pub ty: String,
    pub value: String,
    /// The process instance's own contribution before child aggregation.
    /// Absence means the process contributes no local value.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub direct_value: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub direct_expression: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub display_unit: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub expression: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub bounds: Option<MetricBounds>,
    pub aggregate: String,
    pub span: SourceSpan,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub dependencies: Vec<String>,
}

/// A conservative temporal reading of a monitored, time-valued makespan.
///
/// `known_work` is intentionally separate from the duration of the whole
/// design.  It describes only the work whose duration is currently defined.
/// When `completion_upper` is absent, an open duration obligation prevents the
/// compiler from presenting that range as a bound for successful completion.
#[derive(Clone, Debug, Serialize)]
pub struct TemporalMetric {
    /// Stable instance-qualified monitored export path, for example
    /// `Factory.batch.duration`.
    pub path: String,
    /// The concrete display unit inherited from the relevant `monitor`, when
    /// one was specified.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub display_unit: Option<String>,
    /// Bounds for the currently-defined part of the process graph.  Exact
    /// values use identical lower and upper strings.
    pub known_work: TemporalRange,
    /// A lower bound for successful completion, when it can be established.
    /// This is omitted rather than guessed when an open contribution might
    /// invalidate it.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub completion_lower: Option<String>,
    /// An upper bound for successful completion.  An omitted value means that
    /// the graph is incomplete or otherwise cannot establish an upper bound.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub completion_upper: Option<String>,
    /// `exact`, `bounded`, `symbolic`, or `incomplete`.
    pub status: String,
    /// The scheduling reason for a non-exact known-work range, if applicable.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    /// Open obligations that prevent this temporal value from describing full
    /// successful completion.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub obligations: Vec<String>,
}

#[derive(Clone, Debug, Serialize)]
pub struct TemporalRange {
    pub lower: String,
    pub upper: String,
    /// Exact base-second values for timeline consumers. The author-facing
    /// strings above retain the selected display unit.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub lower_seconds: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub upper_seconds: Option<String>,
}

/// An earliest-availability declaration for a process stream export.
///
/// The offset is relative to the producer run's start. A consumer can begin
/// after this event and may then overlap the producer; it is not required to
/// wait for the producer's whole stream to close.
#[derive(Clone, Debug, Serialize)]
pub struct StreamHandoff {
    /// Stable instance-qualified producer path, for example
    /// `Factory.batch.reworking`.
    pub path: String,
    /// Name of the producer's `Stream[T]` export.
    pub stream: String,
    /// Author-facing earliest-first-item delay.
    pub first_item_after: String,
    /// Exact base-second delay, when statically known.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub first_item_after_seconds: Option<String>,
    /// `exact` when the delay is statically known, otherwise `symbolic`.
    pub status: String,
    pub span: SourceSpan,
}

/// Planned finite cardinality for one instantiated stream export.
#[derive(Clone, Debug, Serialize)]
pub struct StreamCardinality {
    pub path: String,
    pub stream: String,
    pub cardinality: String,
    pub status: String,
    pub span: SourceSpan,
}

/// A finite repeated-job schedule derived from effectful `for` / `repeat`
/// bodies, their data and causal edges, and explicit capacity-resource loans.
/// Unlike `TemporalMetric`, this is an operational schedule rather than a
/// conservative algebraic bound.
#[derive(Clone, Debug, Serialize)]
pub struct ResourceSchedule {
    pub path: String,
    pub jobs: u64,
    pub repetitions: u64,
    pub operation_count: u64,
    /// Absolute offset from the root schedule at which every required
    /// provisioned pool for this process is available.
    pub start_offset_seconds: String,
    pub makespan: String,
    pub makespan_seconds: String,
    pub status: String,
    pub stages: Vec<ResourceScheduleStage>,
    pub resources: Vec<ResourceUtilization>,
    pub span: SourceSpan,
}

#[derive(Clone, Debug, Serialize)]
pub struct ResourceScheduleStage {
    pub path: String,
    pub operation_count: u64,
    pub duration: String,
    pub duration_seconds: String,
    pub minimum_duration_seconds: String,
    pub maximum_duration_seconds: String,
    /// Earliest time any operation of this stage became causally ready,
    /// including time spent queued for finite resource capacity.
    pub earliest_ready_seconds: String,
    pub earliest_start_seconds: String,
    pub latest_finish_seconds: String,
    /// Compact time buckets distinguishing service work, stream-close waits,
    /// and ready-but-not-dispatched backlog across repeated operations.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub activity: Vec<ResourceScheduleActivity>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub resources: Vec<String>,
}

#[derive(Clone, Debug, Serialize)]
pub struct ResourceScheduleActivity {
    pub start_seconds: String,
    pub end_seconds: String,
    /// Mean concurrently executing operations during this bucket.
    pub average_active_operations: String,
    /// Mean operations whose own work finished but whose run remained open
    /// waiting for an input stream to close.
    pub average_waiting_operations: String,
    /// Mean causally-ready operations waiting for resource capacity.
    pub average_queued_operations: String,
}

#[derive(Clone, Debug, Serialize)]
pub struct ResourceUtilization {
    pub path: String,
    pub stage: String,
    pub capacity: String,
    pub claim_per_operation: String,
    pub parallel_capacity: u64,
    pub peak_claim: String,
    /// Exact claim × occupied-seconds contribution for this stage. Consumers
    /// can sum this across every stage and schedule that shares `path`.
    pub busy_capacity_seconds: String,
    pub utilization: String,
}

/// One concrete reusable capacity pool provisioned by a process instance.
#[derive(Clone, Debug, Serialize)]
pub struct ProvisionedResource {
    pub path: String,
    pub resource: String,
    pub capacity: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub available_after_seconds: Option<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub aliases: Vec<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub borrowers: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub utilization: Option<String>,
    pub span: SourceSpan,
}

#[derive(Clone, Debug, Serialize)]
pub struct Report {
    pub schema_version: u32,
    pub status: Completeness,
    pub entry: String,
    pub modules: Vec<String>,
    pub diagnostics: Vec<Diagnostic>,
    pub symbols: Vec<SymbolReport>,
    pub obligations: Vec<Obligation>,
    pub constraints: Vec<Constraint>,
    pub provenance: Vec<ProvenanceRecord>,
    pub assumed_zero: Vec<AssumedZero>,
    pub monitored_metrics: Vec<MonitoredMetric>,
    /// Conservative temporal projections for monitored time makespans.
    /// Additive within report schema version 1.
    pub temporal_metrics: Vec<TemporalMetric>,
    /// Per-stream earliest-emission events used by temporal consumers.
    /// Additive within report schema version 1.
    pub stream_handoffs: Vec<StreamHandoff>,
    /// Planned cardinalities for instantiated stream exports.
    /// Additive within report schema version 1.
    pub stream_cardinalities: Vec<StreamCardinality>,
    /// Exact finite schedules derived from physical loops and capacity claims.
    /// Additive within report schema version 1.
    pub resource_schedules: Vec<ResourceSchedule>,
    /// Reusable resource pools created by process runs.
    /// Additive within report schema version 1.
    pub provisioned_resources: Vec<ProvisionedResource>,
}

impl Report {
    pub fn is_failure(&self, require_complete: bool) -> bool {
        self.status == Completeness::Invalid
            || (require_complete && self.status == Completeness::Partial)
    }

    pub fn recompute_status(&mut self) {
        self.status = if self
            .diagnostics
            .iter()
            .any(|diagnostic| diagnostic.severity == Severity::Error)
        {
            Completeness::Invalid
        } else if self.obligations.is_empty() {
            Completeness::Complete
        } else {
            Completeness::Partial
        };
    }
}
