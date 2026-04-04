# Hello World

[`hello.req`](hello.req) defines one physical artifact and one process. The
process takes ownership of a sample, constrains its mass, and transfers the same
artifact to an export.

From the repository root:

```console
cargo run -- check docs/examples/HelloWorld/hello.req
```

The result is `complete`: the model has no missing exports or connections. Its
mass requirement is still shown as a residual constraint because the model does
not provide one particular sample mass. Residual constraints describe the valid
input space; they do not make a model partial.

Try replacing `0 Kilogram` with `0 Second`. The comparison then mixes mass and
time, so the analyzer reports a dimensional type error.

Ownership is equally important. `verified = sample` is a move. A second export
of the same `sample` would be rejected as a double move.
