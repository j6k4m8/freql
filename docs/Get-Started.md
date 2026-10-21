# Get started with freql

freql describes process flows as contracts over owned items, reusable resources, copyable values, and explicit connections. This guide uses packaging and workshop examples. The compiler analyzes the model without executing it.

## Build and check a model

Build with Rust 1.91 or newer:

```sh
cargo build
cargo run -- check docs/examples/HelloWorld/hello.req
cargo run -- check docs/examples/Basic/main.req
```

Use `cargo install --path .` to install `freql` locally. The rest of this guide uses that executable directly.

A check has three possible results:

| Status | Meaning |
| --- | --- |
| `complete` | Valid model with no open obligations. |
| `partial` | Valid model with missing configuration, definitions, outputs, or connections. |
| `invalid` | Syntax, type, unit, ownership, wiring, or contradiction error. |

A symbolic requirement on an input is a residual constraint, not a missing definition. A model can therefore be complete while retaining constraints on the items it accepts. A requirement proven false is an error.

`check` exits with `0` for complete or partial models, `1` for invalid models or partial models under `--require-complete`, and `2` for command-line or unrecoverable I/O errors.

## Statement layout and values

A newline ends a complete expression. Semicolons are optional. Parentheses and brackets, an unfinished operator, or a leading member-access `.` permit continuation:

```req
let volume = width *
    height * length
let duration = (
    setup_time
    + processing_time
)
let total = result
    .duration
```

An opening `(` or `[` on the next line begins a new statement. Keep a call's opening delimiter on the same line as its callee, including chained configuration and invocation: `Verifier(config)(inputs)`. Comments and blank lines retain these layout rules. `else` still attaches to its `if` across a newline.

Use `let name = value` for immutable locals and `let mut name = value` for mutable locals. There is no `var` declaration. `#` introduces a line comment, and triple-quoted strings document declarations.

Quantities, `Integer`, `Scalar`, `Bool`, and `String` are copyable. Strings use double quotes and support `\n`, `\r`, `\t`, `\0`, `\"`, and `\\`. They support equality and inequality but are incompatible with numeric quantities.

## Artifacts and ownership

An `artifact` has identity and can own other artifacts. Stored fields describe state; derived fields are read-only expressions:

```req
artifact Workpiece {
    width: Length
    height: Length
    thickness: Length

    volume: Volume {
        width * height * thickness
    }
}

artifact Bundle {
    items: Workpiece[]
}
```

Moving a `Bundle` moves its contained array and all its workpieces. Artifact construction, collection insertion, wiring, `yield`, and export assignments transfer owned values. The analyzer rejects reuse after a move and double transfers.

A process receives ownership with `gets` and returns ownership through `export`:

```req
artifact Parcel {
    mass: Mass
}

process VerifyParcel {
    gets parcel: Parcel
    export verified: Parcel

    () => {
        requires parcel.mass > 0 Kilogram
        verified = parcel
    }
}
```

An input that is neither exported nor incorporated into an exported artifact is consumed by that run. Provenance records follow possible lineage through construction, containment, aliases, collection insertion, and by-value loops. They describe static production sites rather than an event for each runtime item.

## Exact units and dimensional arithmetic

Built-in dimensions and units include SI bases, `Area`, `Volume`, and `USD`. Define derived dimensions and scaled units with ordinary expressions:

```req
unit Speed = Length / Time
unit Millimeter = Meter / 1000
unit Hour = Second * 3600
unit Information = base
unit Byte = Information
unit Bit = Byte / 8
unit Petabyte = Byte * 1000000000000000
```

`base` creates a fresh nominal dimension. Two separately declared base dimensions remain incompatible even if their scales match. Quantities use exact rational scales rather than floating-point approximations.

Dimensional expressions work directly in annotations, such as `config speed: Length / Time`. A bare unit means exactly one unit. `as Unit` requests a compatible presentation unit without changing the value, type, or constraints.

`130 Millimeter^2` means 130 square millimeters. `(130 Millimeter)^2` raises the whole quantity and means 16,900 square millimeters. Exponents are dimensionless numeric literals and may be signed. A parenthesized coefficient can carry a unit suffix: `(350 / 24) Day` is equivalent to `(350 / 24) * Day` and converts to exactly `350 Hour`. Suffix powers still apply to the unit: `(2 + 1) Meter^2` means three square meters; `((2 + 1) Meter)^2` means nine square meters.

`Integer` and `Scalar` are dimensionless. After `import math`, `math.pi` and unresolved roots can remain symbolic. `math.ceil` and `math.floor` round in the resolved unit's exact scale: `math.ceil(1.2 Millimeter)` is `2 Millimeter`, while `math.ceil(3.2)` is an integer. Equality is exact; tolerances and uncertainty are not implicit.

### Numeric refinements

Refinements add reusable requirements to numeric types:

```req
unit PositiveScalar = Scalar(x) {
    requires x > 0
}

unit Fraction = PositiveScalar(x) {
    requires x <= 1
}

unit PositiveLength = Length(x) {
    requires x > 0 Meter
}
```

`Fraction` inherits both predicates, giving `0 < x <= 1`. Refinements retain their numeric representation and scale. Arithmetic produces ordinary numeric expressions that must satisfy a refinement again when assigned to a refined annotation.

Checks apply to configuration, invocation inputs, exports, typed locals and assignments, function parameters and results, numeric array elements, artifact fields, and stream elements. Integer refinements also require integrality. Unknown values retain predicates as constraints; concrete contradictions are errors.

Refinement bodies contain only requirements over their binder and visible units. They do not introduce literal suffixes or refine whole artifacts. Refinements in user-trait signatures and implementations are currently rejected. Declaration-default calls to refined functions require statically satisfied predicates. Stateful symbolic pure-function loops have additional restrictions; concrete loops of up to 1024 items and stateless per-item checks are supported. See the [refinement example](examples/Basic/refinements.req).

## Configuration, invocation, and contracts

A process has two distinct calls. Calling the declaration creates a configured process; calling that value creates a lazy run node:

```req
let verifier = VerifyParcels(max_parcel_mass: limit)
let verify = verifier()
```

Stable parameters use `config`. Per-run copyable parameters belong to the `(...) =>` signature. Physical ownership enters through `gets`, rather than invocation arguments. `Verifier(config)(inputs)` is the chained form of the same two calls.

Configuration may have defaults. `expose config` additionally permits CLI and browser overrides:

```req
process Plan {
    expose config width: Length = 2 Meter
    config internal_multiplier: Scalar = 2
    export planned_width: Length

    (allowance: Length) => {
        planned_width = width * internal_multiplier + allowance
    }
}
```

Defaults and exposure are independent. Ordinary `config` can be required or defaulted, and so can exposed configuration. Defaults may depend on other configuration fields in any declaration order, but not on physical ports or invocation parameters. Cyclic dependencies are errors. Supplying a value replaces the default. Unmarked typed process members are rejected.

A same-name process argument can use shorthand: `VerifyParcels(max_parcel_mass)` means `VerifyParcels(max_parcel_mass: max_parcel_mass)`. This applies to configuration and invocation calls. It does not change pure-function argument semantics.

A process may inherit one or more contracts using `process Implementation: Contract`. Inherited fields and obligations must be fulfilled. Executable parent effects, such as child runs, ownership transfers, or mutation, are not inherited. Compatible inherited configuration coalesces, preserving exposure when a parent exposes it.

A configuration field may accept a preconfigured implementation of a process contract:

```req
process Inspection {
    gets parcel: Parcel
    export verified: Parcel
}

process Workflow {
    config inspection: Configured[Inspection]

    () => {
        let inspect = inspection()
        # Connect the run's physical ports here.
    }
}
```

Pass a configured concrete process that inherits `Inspection`. The compiler resolves the implementation while elaborating the graph; this is a static strategy choice.

## Requirements and expressions

`requires` accepts a dimensionally valid `Bool` expression:

```req
requires parcel.mass > 0 Kilogram and parcel.mass <= max_parcel_mass
requires lane_count == 1 or lane_count == 4
requires not (mode == "draft")
```

`not` and `!` are equivalent. From loosest to tightest, precedence is `or`, `and`, prefix `not`/`!`, equality, ordered comparisons, addition/subtraction, multiplication/division, and exponentiation. Exponentiation associates right; binary operators in the other tiers associate left. Thus `not a > b` means `not (a > b)`.

The compiler folds decidable predicates, including Boolean short-circuit cases, and substitutes configured values through child instances. Remaining predicates retain their run paths, source spans, and dependencies.

## Streams, arrays, and finite loops

`Stream[T]` is linear and has one consumer. A producer transfers items with `yield`, and normal producer completion closes the stream:

```req
import math

process MakeParcels {
    export parcels: Stream[Parcel]

    (count: Integer, mass: Mass) => {
        requires count >= 0
        requires mass > 0 Kilogram
        for i in math.range(0, count) {
            yield parcels = Parcel(mass: mass)
        }
    }
}
```

`math.range(start, end)` takes positional integer bounds and is half-open. Negative bounds are allowed; `end <= start` is empty. There is no `while`. Requirements inside finite array, range, and stream loops describe every item; an empty range satisfies them vacuously.

A consumer can collect ownership in an array:

```req
let mut collected: Parcel[] = []
for parcel in parcels {
    collected.append(parcel)
}
```

`append` moves the item. Moving that array into a bundle transfers all collected ownership. A mutable accumulator can be reset to `[]` after it is moved.

An optional first-item latency uses `yield output = value after 12 Second`. This allows a downstream consumer to start after the handoff and overlap its producer, but it cannot finish before its input closes. It does not specify item cadence or buffering.

`std.stream.zip` pairs named input streams:

```req
import std.stream

for pair in std.stream.zip(parcel: parcels, label: labels) {
    yield labeled = LabeledParcel(parcel: pair.parcel, label: pair.label)
}
```

The named arguments become owned pair fields. A direct loop stops when either input closes and leaves both stream cursors available for subsequent tail consumption. `zip_exact` instead requires equal successful closure. Neither intrinsic implies backpressure or buffer capacity.

`requires cardinality(parcels) == count` records a planned job count without making a stream an in-memory array.

Finite loops can construct and wire child runs. Those runs represent actual repeated work in the model. Fresh iteration inputs can move through each run; a captured unrelated artifact cannot be moved repeatedly. A loop that is not provably single-iteration cannot define a non-stream export or directly mutate stored artifact state.

`repeat count { ... }` elaborates repeated process statements within one job. A loop-carried owned value must be returned on every path before the next repetition can use it. Returned identity can order successive operations; other independent tails can overlap when their dependencies and resources allow. Completion of the repeat still waits for all its operations.

## Connections, loans, and causal order

Composition connects physical ports explicitly:

```req
incoming -> verify.incoming
verify.verified -> dispatch.ready
dispatch.batch -> batch
```

Each required child input needs a source. Owned artifacts and streams have one destination. Missing connections and unfulfilled owned outputs become obligations where representable; incompatible types, invalid port directions, and repeated ownership transfers are errors.

`first before second` orders runs without transferring a value. Causal, import, and process-inheritance cycles are errors.

`uses tool: Tool` exclusively borrows a reusable artifact for a run. A borrower may read or explicitly mutate stored fields, but cannot transfer the artifact's ownership. Two unordered read-only exclusive borrowers produce a residual `non_overlap(...)` constraint. If either mutates the artifact, explicit causal ordering is required. See the [advanced example](examples/Advanced/README.md).

## Capacity resources and provisioning

A resource defines a divisible capacity ceiling. A `uses` claim is a loan held for the run's full interval:

```req
resource Workstation {
    capacity: 10
}

process Task {
    uses station: Workstation[1]
    () => {}
}
```

Claims must be positive, dimensionally compatible with the resource, and within its capacity. Omitting brackets claims the whole declared capacity. Physical quantities work too: a resource with `capacity: 100 Meter` can be claimed as `[10 Meter]`.

A type declaration alone does not create available capacity. A process provisions a pool by directly fulfilling a matching export:

```req
import std.process (DurationAccounting)

process ProvisionFacility: DurationAccounting {
    config station_count: Integer
    config setup_duration: Time
    export stations: Workstation[station_count]

    () => {
        requires station_count > 0
        stations = Workstation[station_count]
        duration = setup_duration
    }
}
```

That constructor form is allowed only as the matching direct export assignment. Resources cannot be stored in artifacts, bound to locals, passed to pure functions, used as ordinary configuration or invocation values, or received with `gets`.

After the producing run completes, its pool can feed multiple `uses` ports:

```req
let facility = ProvisionFacility(station_count: 4, setup_duration: 60 Second)()
let first = Task()()
let second = Task()()
facility.stations -> first.station
facility.stations -> second.station
```

Fan-out shares the same capacity. Re-exporting through a composite preserves the original pool identity. A claim may depend on invocation values and is checked against both the type ceiling and the actual wired pool. A bodyless resource-export contract remains partial until a concrete construction fulfills it.

Exact resource scheduling requires finite compile-time job and repeat counts. Zero counts are allowed; negative counts are errors. Schedules include operation counts, makespan, stage spans, utilization, and activity profiles. Materialized items can retain distinct durations. Borrowers in different child composites still compete for the same originating pool. Timed stream consumers hold capacity from their first-item handoff until their input closes and their own work completes. See the [provisioned-facility example](examples/Provisioned-Facility.req).

## Copyable tuples, functions, and traits

Named tuples group copyable settings, measurements, or results without declaring a new type:

```req
process PlanDimensions {
    config settings: (scale: Scalar, margin: Length)
    export result: (width: Length, valid: Bool)

    (width: Length) => {
        result = (
            width: width * settings.scale + settings.margin,
            valid: width > 0 Meter,
        )
    }
}
```

Access fields with `.field`. Fields match by name regardless of order; nested tuples, single-field tuples, and arrays of copyable values are supported. Duplicate, missing, extra, or dimensionally incompatible fields are rejected. Tuples cannot contain artifacts, streams, resources, or process values, even through nested arrays. Numeric tuple fields can use refinements. There is no tuple field mutation. See the [named-tuple example](examples/Basic/named-tuples.req).

Pure functions use named result fields:

```req
fn perimeter(width: Length, height: Length) -> (distance: Length) {
    distance = 2 * (width + height)
}
```

The result is a structural tuple, so `perimeter(width, height).distance` selects its value. Pure functions support finite loops, local mutation, symbolic branches, and generic bounds. They cannot construct, move, mutate, or return owned artifacts. Recursive calls are rejected, and every result path must be fulfilled.

Generic parameters require explicit capabilities for their operations:

```req
fn identity[T](value: T) -> (result: T)
where T: Copyable {
    result = value
}

trait Accepted[T] {
    fn accept(value: T) -> (result: Bool)
}

impl Accepted[Scalar] {
    fn accept(value: Scalar) -> (result: Bool) {
        result = value >= 0
    }
}
```

Trait implementations and dispatch are intentionally narrow. Generic tuple constituents require `Copyable` bounds. `std.collection` supplies pure collection helpers. The compiler checks function bodies and capability bounds, but does not yet propagate every pure-function precondition through every composite call path.

## Accounting and monitored metrics

The embedded standard library supplies ordinary process contracts:

```req
import std.process (CostAccounting, DurationAccounting)

process Pack: CostAccounting, DurationAccounting {
    monitor cost as USD
    monitor duration as Second

    () => {
        cost = 2 USD
        duration = 10 Second
    }
}
```

`CostAccounting` exports `cost: USD aggregate sum`; `DurationAccounting` exports `duration: Time aggregate makespan`. An assignment supplies the process's direct contribution. Composite totals include child totals automatically, once per direct child.

| Aggregate | Meaning |
| --- | --- |
| `sum` | Direct contribution plus direct-child totals. |
| `max`, `min` | Reducer over the direct contribution and child totals. |
| `makespan` | Child event-graph makespan with direct duration as serialized overhead. |
| `explicit` | Assignment defines the entire value; no automatic child rollup. |

Do not read a child aggregate to define a direct contribution: the compiler rejects that double counting. Use `aggregate explicit` for a manual reducer. Work intended to overlap belongs in separate child runs with explicit dependencies.

A composite with no direct assignment contributes the reducer identity while still accounting for children. A leaf with no definition remains partial. `ZeroCost` and `ZeroDuration` express intentional zero contributions. Pure inherited aggregate definitions are overridable defaults, rather than extra charges. Identical inherited defaults coalesce; conflicting ones are errors.

`monitor metric` requests reporting and coverage: descendant runs must expose that metric through compatible composites. Missing coverage is an obligation rather than assumed zero. `monitor speed: Length / Time` is shorthand for an explicit export plus its monitor. Monitors are inherited, and a local clause can override the inherited presentation unit.

`monitor cost as MillionUSD` uses an exactly compatible unit for display only. An outer display choice propagates to matching descendant metrics. Reports show the total, any direct contribution, source, type, rollup rule, dependencies, and optional display unit.

When scheduling remains unresolved, a makespan may retain a symbolic expression and conservative lower/upper bounds. These are possible-overlap bounds rather than predicted durations. The JSON `temporal_metrics` projection distinguishes currently defined work from successful completion; missing child durations prevent a completion bound from being asserted.

## Modules and names

Selective imports use quoted relative paths or the embedded library:

```req
import "artifacts.req" (Parcel, DispatchBatch)
import std.process (CostAccounting, DurationAccounting)
import math
```

Quoted paths resolve relative to their importing file. Namespace imports avoid long member lists:

```req
import "tools.req" as tools
```

Then use `tools.Name` for the module's exported unit, artifact, resource, process, trait, refinement, or function. Qualified units also work in quantity suffixes and compound annotations. Two aliases of the same module retain declaration identity. Private imported bindings are not re-exported implicitly.

Bare quoted imports and empty selective imports are errors. Aliases must not collide with declarations, other imports, or reserved native namespaces `math` and `std`. There are no implicit namespaces or glob imports.

Parameters, locals, and loop variables cannot reuse visible value names. A `unit` declaration silently replaces a built-in of the same name; duplicate unit declarations in the loaded module set are errors (`E0201`), and ambiguous imported declarations are rejected. See the [namespace example](examples/Basic/namespaces/main.req).

## Calculator

Start the persistent dimensional calculator with `freql calc`, or select an import base with `freql calc --root DIRECTORY`:

```text
⁂ unit Hour = Second * 3600
⁂ unit Information = base
⁂ unit Byte = Information
⁂ unit Gigabyte = Byte * 1000000000
⁂ unit Terabyte = Byte * 1000000000000
⁂ let rate: Information / Time = 3 Gigabyte / Second
⁂ rate * 5 Hour as Terabyte
54 Terabyte
```

The session uses ordinary language parsing, typing, units, and exact presentation conversion. Quoted imports resolve from the calculator root. `:bindings`, `:clear`, `:help`, and `:quit` manage the session. Up/down browse history, Ctrl-R searches it, and Tab completes commands, bindings, declarations, units, and `.req` import paths. Interactive input supports syntax highlighting; piped input omits prompts and terminal formatting.

## CLI, reports, formatting, and browser views

```text
freql check ENTRY [--json] [--require-complete] [--absolute-paths] [--set PATH=EXPR]...
freql fmt FILE... [--check]
freql inspect ENTRY --stage cst|ast|hir|flow|constraints|temporal [--set PATH=EXPR]...
freql render ENTRY [--dot | --html] [--process PROCESS] [--absolute-paths] [--set PATH=EXPR]... [-o FILE]
freql serve ENTRY [--process PROCESS] [--host HOST] [--port PORT] [--no-open] [--absolute-paths] [--set PATH=EXPR]...
freql lsp
freql calc [--root DIRECTORY]
```

Only exposed root configuration can be overridden. Override values are ordinary expressions with the target module's visible names and units:

```sh
freql check docs/examples/Basic/main.req \
  --set 'VerifyAndDispatch.max_parcel_mass=12 Kilogram'
```

Repeat `--set` for multiple fields and quote arguments containing spaces. Defaults do not grant exposure. An attempted override of internal configuration produces a source-located diagnostic.

`check --json` emits a report with `schema_version: 1`, diagnostics, symbols, obligations, monitored metrics, residual constraints, spans, provenance, aggregation assumptions, and additive scheduling information. Prefer that versioned report for integrations. `inspect` stages are unstable compiler-development views.

`fmt` uses four-space indentation, consistent spacing, a 100-column soft target, and newline-separated statements. It preserves comments, literal contents, and blank-line groups. It reparses output and compares syntax trees, refusing malformed input or a change in meaning. `--check` returns a failure when formatting would change a file.

```sh
freql render docs/examples/Basic/main.req --dot
freql render docs/examples/Basic/main.req --html -o flow.html
freql serve docs/examples/Basic/main.req
```

`render` defaults to DOT. HTML is a self-contained interactive snapshot. `serve` watches source changes and refreshes the browser graph and report, with session-only editing of exposed configuration. It binds to `127.0.0.1` and a free port by default; `--no-open` suppresses automatic browser opening. Invalid edits retain the last valid topology with current diagnostics and a stale indication. Resetting browser overrides restores declared defaults or an unset field, without editing source.

Physical transfers appear as solid edges and `before` dependencies as dashed edges. Named ports can be shown or hidden. A Gantt view uses the conservative schedule implied by dependencies, durations, resource claims, and declared first-item handoffs. It does not invent missing stream cadence. `--process NAME` filters graph topology, while the report still covers the entire checked entry and import closure.

`render` emits a best-effort graph and exits `1` for an invalid model. `serve` remains active through invalid live states and exits `0` after a clean shutdown.

## Editor setup

Run `freql lsp` for a compiler-backed Language Server Protocol server over standard input/output. The [VS Code extension](../editors/vscode/README.md) provides highlighting, snippets, diagnostics, contextual completion, process signature help, hover, navigation, formatting, conservative local rename, and supported quick fixes.

Use a workspace entry when editing shared modules:

```json
{
  "freql.entry": "docs/examples/Basic/main.req"
}
```

The selected entry is resolved relative to the workspace or as an absolute path. Analysis uses open buffers, including unsaved imported files. Files outside that import closure are checked independently. Empty restores per-file behavior. Untitled documents receive highlighting and snippets but require a file path for compiler analysis. See [editor support](../editors/README.md) for portable TextMate integration.

## Current boundaries

freql is experimental. It checks a useful subset of structural, dimensional, ownership, provenance, capacity, and scheduling rules, with conservative obligations when content is missing.

The analyzer does not execute processes, optimize designs, provide source-query syntax, model buffered-stream capacity or backpressure, or add tolerance and uncertainty semantics. Full failure-state and loan-lifetime semantics, precise sibling-field and sibling-index ownership after a move, general effect merging, complete pure-function precondition propagation, broad trait dispatch, and `std.stream.Partition[T, N]` remain deferred. Moving an owned subfield or indexed element currently marks its entire root artifact or array as moved.

The language contract intends borrowed field mutations to survive failure and loans to be released on either success or failure; the analyzer does not yet represent that full lifecycle. A valid static model should be read within these implemented boundaries.
