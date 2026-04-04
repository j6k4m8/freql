'use strict';

const fs = require('fs');
const path = require('path');

const repository = path.resolve(__dirname, '..', '..', '..');
const lsp = fs.readFileSync(path.join(repository, 'src', 'lsp.rs'), 'utf8');
const ide = fs.readFileSync(path.join(repository, 'src', 'ide.rs'), 'utf8');
const extension = fs.readFileSync(path.join(__dirname, '..', 'extension.js'), 'utf8');

if (!extension.includes("args: ['lsp']")) throw new Error('VS Code must launch the Rust LSP server.');
for (const duplicate of [
  'registerCompletionItemProvider',
  'registerHoverProvider',
  'registerSignatureHelpProvider',
  'registerDocumentSymbolProvider'
]) {
  if (extension.includes(duplicate)) throw new Error(`Editor duplicates compiler semantics: ${duplicate}`);
}

for (const trigger of ['".".into()', '":".into()', '"(".into()', '",".into()']) {
  if (!lsp.includes(trigger)) throw new Error(`Rust LSP does not advertise completion trigger ${trigger}.`);
}
if (!/snapshot\s*\.completion_at/.test(lsp)) throw new Error('LSP completion does not delegate to the compiler snapshot.');
if (!/snapshot\s*\.signature_at/.test(lsp)) throw new Error('LSP signature help does not delegate to the compiler snapshot.');
if (!lsp.includes('signature_help_provider')) throw new Error('Rust LSP does not advertise signature help.');

for (const compilerCase of [
  'Type::Artifact',
  'Type::ConfiguredProcess',
  'Type::Run',
  'CompletionKind::Configuration',
  'CompletionKind::Parameter',
  'CompletionKind::Port'
]) {
  if (!ide.includes(compilerCase)) throw new Error(`Compiler completion is missing ${compilerCase}.`);
}
for (const regressionTest of [
  'overlay_and_artifact_member_completion_use_compiler_types',
  'completes_process_configuration_invocation_and_run_ports'
]) {
  if (!ide.includes(regressionTest)) throw new Error(`Missing compiler regression test ${regressionTest}.`);
}

console.log('Validated thin-client boundary and Rust member-completion contract.');
