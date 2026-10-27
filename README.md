# freql

freql is an experimental, general-purpose process-flow language. Describe the items a process owns, the resources it borrows, the outputs it produces, and the requirements that must hold. Compose processes into a checked flow graph with exact units, explicit ownership, finite streams, and cost or duration accounting.

The compiler is a static analyzer. It checks models and exposes unresolved requirements; it does not execute processes or optimize a plan.

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

`gets` receives ownership. Assigning `parcel` to `verified` transfers that ownership to the output. A second transfer of the same parcel is an error.

## Try it

Build with Rust 1.91 or newer:

```sh
cargo build
cargo run -- check docs/examples/HelloWorld/hello.req
cargo run -- check docs/examples/Basic/main.req
cargo run -- calc
```

Install the CLI locally with `cargo install --path .`. Models use the `.req` extension.

The analyzer reports `complete` for valid models with no open obligations, `partial` for valid models with missing values or connections, and `invalid` for models with errors. A residual constraint describes allowed inputs; it does not make a model partial. Use `--require-complete` when missing model content should fail a check.

## Learn the language

- [Get started and language guide](docs/Get-Started.md)
- [Hello World: ownership and constraints](docs/examples/HelloWorld/README.md)
- [Basic: a multi-file streaming pipeline](docs/examples/Basic/README.md)
- [Advanced: units, state, and ordering](docs/examples/Advanced/README.md)
- [Provisioning shared capacity](docs/examples/Provisioned-Facility.req)
- [Dimensional calculation](docs/examples/Dimensional-Calculator.req)
- [Editor setup](editors/README.md)

## Tools

```text
freql check ENTRY [--json] [--require-complete] [--absolute-paths] [--set PATH=EXPR]...
freql fmt FILE... [--check]
freql inspect ENTRY --stage cst|ast|hir|flow|constraints|temporal [--set PATH=EXPR]...
freql render ENTRY [--dot | --html] [--process PROCESS] [--absolute-paths] [--set PATH=EXPR]... [-o FILE]
freql serve ENTRY [--process PROCESS] [--host HOST] [--port PORT] [--no-open] [--absolute-paths] [--set PATH=EXPR]...
freql lsp
freql calc [--root DIRECTORY]
```

`check --json` produces a schema-versioned report for tools and CI. `render` produces Graphviz DOT or a self-contained interactive HTML snapshot. `serve` watches source files and updates a live browser view; exposed configuration can be edited for that server session. `lsp` supplies compiler-backed editor features over standard input/output. `inspect` exposes unstable compiler representations.

```sh
cargo run -- render docs/examples/Basic/main.req --html -o flow.html
cargo run -- serve docs/examples/Basic/main.req \
  --set 'VerifyAndDispatch.max_parcel_mass=12 Kilogram'
```

The calculator retains imports and immutable bindings and uses the model language's exact dimensional arithmetic:

```text
⁂ unit Hour = Second * 3600
⁂ let speed: Length / Time = 5 Meter / Second
⁂ speed * 2 Hour as Meter
36000 Meter
```

Use `as ~` for decimal presentation in the inferred unit, or `as ~Hour` to
select a compatible unit. For example, `(2 Hour / 3) as ~Hour` displays
`≈0.666667 Hour`, while plain `as Hour` retains `2/3 Hour`. Calculations remain
exact; monitors support the same presentation syntax. `as` also works in normal
code, including bindings and function arguments. Parenthesize a converted operand
when continuing arithmetic: `(2 Hour as Minute) + 30 Second`.

## Development checks

From the repository root:

```sh
cargo test --locked --all-targets
cargo build --locked
(cd editors/vscode && npm ci && npm test && npm run test:server)
```

The editor integration test launches the built CLI and checks the LSP handshake,
formatting of an unsaved buffer, and shutdown. Set `FREQL_TEST_BINARY` to use a
binary built in a custom Cargo target directory.

## Current boundaries

The analyzer supports structural, dimensional, ownership, resource-capacity, provenance, and selected scheduling checks. It deliberately leaves unresolved obligations visible. General logical proofs, full failure and loan-lifetime semantics, buffered streams and backpressure, execution, and design optimization remain outside its current scope. See the [language guide](docs/Get-Started.md#current-boundaries) for details.
