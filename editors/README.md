# freql editor support

The compiler's language server supplies shared semantic behavior for `.req` process-flow models. Start it over standard input/output with:

```sh
freql lsp
```

## VS Code

The [VS Code extension](vscode/README.md) launches the server and provides highlighting, snippets, compiler diagnostics, contextual completion, process signature help, hover, navigation, formatting, conservative local rename, and supported quick fixes.

Build the compiler from the repository root with `cargo build`. The extension uses an explicit `freql.server.path` setting first, the active workspace's `target/debug/freql` second, and `freql` from `PATH` last. Use **freql: Restart Language Server** after rebuilding or changing the executable.

For shared modules, choose an entry model in workspace settings:

```json
{
  "freql.entry": "docs/examples/Basic/main.req"
}
```

The server analyzes that entry's import closure with current unsaved buffers. Empty restores independent per-file analysis. Compiler-backed features require file-backed documents; untitled documents still receive highlighting and snippets.

## Other editors

Configure an LSP client to launch `freql lsp` for `.req` files. Supported compiler semantics are implemented in Rust, so clients do not need an independent parser or analyzer.

The [TextMate grammar](vscode/syntaxes/freql.tmLanguage.json) uses scope `source.freql` and can be installed independently in editors that accept TextMate JSON. Associate that scope with `.req`. The [language configuration](vscode/language-configuration.json) supplies reusable bracket, comment, indentation, and word-pattern settings for clients with analogous formats.
