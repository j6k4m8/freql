# Changelog

## 0.1.5

- Fix server startup: use the executable's default stdio transport without injecting an unsupported `--stdio` argument.
- Add launch-option regression coverage and a real-server handshake test using the extension's launch configuration.

## 0.1.4

- Add compiler-backed hover, definitions, references, and conservative local-binding rename.
- Add document formatting, contextual completions, and version-guarded quick fixes.
- Explain constraints using available compiler evidence.
- Add `freql.entry` for checking a selected entry model with unsaved document changes.
- Support namespace imports, numeric refinements, and named tuple types in editor tooling.
- Correct language-server initialization and shutdown handling.

- Highlight and complete explicit `config` and `expose config` declarations.
- Show configuration defaults in compiler-backed completion and process signature help.
- Insert same-name process arguments using freql's bare-name shorthand.
- Add snippets for ordinary/exposed configuration and punned arguments.

## 0.1.3

- Prefer an explicit `freql.server.path`, then an active-workspace `target/debug/freql`, then `freql` from `PATH`.
- Make local compiler development work without mutating editor or shell settings.

## 0.1.2

- Match the first Rust language-server slice: file-backed documents, diagnostics, and completion.
- Stop advertising or selecting unsupported untitled-document and semantic features.
- Document hover, navigation, signatures, and document symbols as planned work.

## 0.1.1

- Replace JavaScript semantic approximations with a thin Language Server Protocol client.
- Launch `freql lsp` from `freql.server.path` or `PATH`.
- Retain TextMate highlighting, static snippets, and the explicit compiler report command.
- Add a language-server restart command and protocol tracing setting.

## 0.1.0

- Add `.req` language registration and portable TextMate grammar.
- Add language configuration and grammar-oriented snippets.
- Add structural completions, hover help, signatures, and document symbols.
- Add optional diagnostics through `freql check --json`.
