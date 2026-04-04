# Basic: parcel verification and dispatch

This example models a dispatch workflow without constructing replacement parcels. Each incoming parcel keeps its identity as it passes verification and joins an owned dispatch batch.

- [`artifacts.req`](artifacts.req) defines a parcel's mass and destination and a dispatch batch that owns an array of parcels.
- [`processes.req`](processes.req) verifies a stream of parcels, then collects the verified stream for dispatch.
- [`main.req`](main.req) configures and connects both runs.

Check the entry module from the repository root:

```sh
cargo run -- check docs/examples/Basic/main.req
```

`VerifyAndDispatch` exposes the maximum allowed parcel mass, with a default of 25 kilograms:

```sh
cargo run -- check docs/examples/Basic/main.req \
  --set 'VerifyAndDispatch.max_parcel_mass=12 Kilogram'
cargo run -- serve docs/examples/Basic/main.req \
  --set 'VerifyAndDispatch.max_parcel_mass=12 Kilogram'
```

The verifier requires a positive mass within that limit and a nonempty destination for each parcel. These are symbolic constraints on accepted inputs; they do not imply missing model content. The model is complete while retaining those constraints.

`incoming -> verify.incoming` transfers the input stream to one consumer. Each `yield verified = parcel` moves the same parcel onward. The dispatcher moves arrivals into its local array with `append`, then moves the array into `DispatchBatch`. The batch is the single owned output. No step duplicates a parcel or makes a physical copy of it.

The live browser displays the flow and exposes the maximum-mass setting for session-only editing. Invalid overrides leave the last valid graph visible with diagnostics. Reset restores the default without changing source.

## More value and module examples

- [`named-tuples.req`](named-tuples.req) shares copyable packaging settings and measurements across child plans. Owned inputs still follow `gets` rules.
- [`refinements.req`](refinements.req) defines positive quantities, a bounded fraction, and positive integers with numeric refinement predicates.
- [`namespaces/main.req`](namespaces/main.req) imports a timer module under an alias, then uses its units, pure helper, and process.

Run any of those entries with `cargo run -- check PATH`.
