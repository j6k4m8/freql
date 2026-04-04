# Advanced: units, state, and ordering

[`model.req`](model.req) demonstrates:

- scaled and derived unit declarations;
- a derived artifact field;
- a named-result pure function;
- exclusive borrowing with `uses`;
- explicit artifact-field mutation; and
- inheritance from the standard `DurationAccounting` contract.

[`main.req`](main.req) composes positioning and measurement and adds a causal
edge with `position before measurement`.

Check it from the repository root:

```console
cargo run -- check docs/examples/Advanced/main.req
```

This model is complete: `MeasureWorkpiece` fulfills its inherited duration
contract, and the remaining requirements are valid symbolic constraints on the
physical inputs.

```console
cargo run -- check docs/examples/Advanced/main.req --require-complete
```

## Borrowed state

`PositionStage` borrows `stage` exclusively and assigns its stored `position`
field. The language contract says the mutation survives process failure and the
owner keeps the stage after the loan is released. The analyzer records the
static field transition and checks conflicts between borrowers; complete
loan-lifetime and failure-state reporting remains deferred.

`position before measurement` states that positioning must finish before
measurement starts even though no value is transferred along that edge. If two
runs mutate the same borrowed resource, this kind of explicit causal ordering is
required. Unordered read-only exclusive users instead produce a residual
`non_overlap(...)` scheduling constraint. Cycles in `before` edges are rejected.

## Exact quantities

`Millimeter` has an exact scale of `Meter / 1000`. `Speed` is a structural
dimension alias. freql compares normalized dimensions while preserving author
spelling for useful diagnostics. `==` remains exact formal equality; physical
tolerance and uncertainty will require future explicit language constructs.

## Features kept out of this runnable example

The analyzer supports fixed-size arrays, symbolic `if/else`, generic pure
functions, and narrow pure-function traits, though this example stays focused
on process behavior. Intrinsic `std.stream.Partition[T, N]` is not implemented
yet. Core streams intentionally do not imply buffering, capacity, degradation,
or backpressure.

The current compiler checks pure-function bodies, results, bounds, and
non-recursion, but does not yet propagate every pure-function `requires` clause
as a caller precondition. Full failure-state and loan-lifetime semantics remain deferred.
