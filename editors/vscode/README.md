# freql Language Support for VS Code

Language support for `.req` files written in **freql**, a general-purpose process-flow language with exact units and explicit ownership.

## Features

- TextMate highlighting for declarations, contracts, ownership roles, constraints, quantities, streams, control flow, comments, and docstrings.
- Grammar-aware static snippets for artifacts, capacity resources, processes,
  pure functions, traits, units, requirements, stream loops, physical repeats,
  wiring, and causal ordering.
- Compiler diagnostics and grammar-aware semantic completion through the Rust freql language server.
- Signature help that distinguishes process configuration from invocation and
  shows configuration defaults.
- Compiler-owned formatting through **Format Document** and VS Code's optional
  format-on-save setting.
- Hover, go-to-definition, references, rename, and **Explain Constraint at Cursor**.
- Quick fixes to expose configuration, qualify exact namespace exports, extend
  existing selective imports, and insert missing named arguments as editable
  `TODO_name` placeholders. Placeholders are deliberately unresolved until filled.
- An explicit **freql: Check Current File** command for viewing the compiler's complete JSON report.

Semantic completion currently covers:

- Artifact fields after an artifact value and `.`.
- Stable configuration arguments while configuring a process.
- Invocation arguments while calling a configured process.
- `gets`, `uses`, and `export` ports after a run value and `.`.
- Provisioned resource exports and their downstream `uses` ports after a run
  value and `.`; resource members use the same compiler-owned process types as
  artifact and stream ports.
- Visible local, configured-process, and run bindings inside a `repeat` block.
- Namespace exports after a namespace alias and `.`, nested tuple fields, and refinement types
  in type annotations. Already supplied named arguments and shorthand arguments
  are omitted, including when earlier values contain tuples or nested calls.

Configuration completion distinguishes exposed configuration and displays
declared defaults. When a compatible caller
binding has the same name as the target field or parameter, completion inserts
the bare-name shorthand (`width`) instead of the redundant `width: width`.

The Rust server advertises `.`, `:`, `(`, and `,` as completion triggers; standard manual completion also works. The extension does not calculate any of these candidates itself.

## Install for development

Build the compiler with `cargo build` from the repository root. Then, from this
extension directory, install its locked dependencies:

```sh
npm ci
```

Open this directory in VS Code, select **Run freql Extension**, and press `F5`.
The included launch configuration opens an Extension Development Host at the
repository root, where the extension can find `target/debug/freql`.

Run the editor checks and real-server integration test from this directory:

```sh
npm test
npm run test:server
```

To build a VSIX after installing dependencies:

```sh
npm run package
```

The extension uses `vscode-languageclient` to speak the standard Language Server Protocol. Packaging uses `@vscode/vsce` as a development dependency.

## Required language server

The extension launches:

```sh
freql lsp
```

The executable resolution order is:

1. The explicit `freql.server.path` setting.
2. `target/debug/freql` (or `freql.exe`) in the active workspace, when present.
3. `freql` from `PATH`.

The workspace-local fallback makes compiler development builds usable without changing global settings. Use **freql: Restart Language Server** after changing the executable or rebuilding it. The extension does not contain a fallback JavaScript analyzer because compiler semantics must have a single implementation.

The manual check command invokes the same executable as:

```sh
freql check path/to/file.req --json
```

## Current limitations

The language server analyzes file-backed `.req` documents using their current
unsaved buffers. Untitled documents without a filesystem path receive TextMate
highlighting and snippets but are not sent to the server. Quick fixes use only
declarations in the loaded import closure; they do not guess imports from arbitrary
workspace files. Missing-argument quick fixes currently target simple argument
lists and leave complex edits to completion.

Choose an entry model for shared modules with a workspace setting:

```json
{
  "freql.entry": "docs/examples/Basic/main.req",
  "[freql]": {
    "editor.defaultFormatter": "freql.freql-language-support",
    "editor.formatOnSave": true
  }
}
```

`freql.entry` accepts an absolute path or a workspace-relative path. Empty means
each opened document is its own entry. The manual check command also uses this
entry when configured. Formatting settings above are optional: the extension
does not change your global settings. Formatting uses four-space indentation,
consistent spacing, a 100-column soft target, and newline-separated statements
without semicolons. It preserves comments, literal contents, and blank-line
groups. It refuses malformed syntax or a layout that changes the parsed program.

Navigation uses declarations and references in the active entry's import closure,
including local/process/artifact declarations and imported names. It does not
claim to search unrelated workspace files. Safe rename currently covers local
`let`/`let mut` and loop bindings; it rejects identifier conflicts, compiler errors,
and bare-name argument shorthand. Member, parameter, namespace alias, and exported
symbol rename remain unavailable until all affected references can be proven.

Hover distinguishes declared defaults from resolved monitored values and labels
resolved values with the entry and instance. **Explain Constraint at Cursor**
shows the actual diagnostic, constraint, or obligation, its source, and the
dependency evidence emitted by the compiler; it is not a complete proof trace.

Bundled platform-specific server binaries are future work. The workspace debug-binary lookup is a developer convenience, not a production distribution mechanism.

The portable TextMate grammar is `syntaxes/freql.tmLanguage.json`; editors that support TextMate grammars can use it independently of VS Code.
