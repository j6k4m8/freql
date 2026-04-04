'use strict';

const fs = require('fs');
const path = require('path');

const root = path.resolve(__dirname, '..');
const jsonFiles = [
  'package.json',
  'language-configuration.json',
  'syntaxes/freql.tmLanguage.json',
  'snippets/freql.json'
];

for (const relative of jsonFiles) {
  const full = path.join(root, relative);
  JSON.parse(fs.readFileSync(full, 'utf8'));
}

const manifest = JSON.parse(fs.readFileSync(path.join(root, 'package.json'), 'utf8'));
for (const grammar of manifest.contributes.grammars) {
  if (!fs.existsSync(path.resolve(root, grammar.path))) throw new Error(`Missing grammar: ${grammar.path}`);
}
for (const snippets of manifest.contributes.snippets) {
  if (!fs.existsSync(path.resolve(root, snippets.path))) throw new Error(`Missing snippets: ${snippets.path}`);
}
if (!manifest.contributes.languages[0].extensions.includes('.req')) throw new Error('The .req extension is not registered.');
if (!manifest.dependencies || !manifest.dependencies['vscode-languageclient']) throw new Error('The thin client must depend on vscode-languageclient.');
if (!manifest.contributes.configuration.properties['freql.server.path']) throw new Error('Missing freql.server.path setting.');
if (manifest.contributes.configuration.properties['freql.checkOnSave']) throw new Error('CLI-on-save checking must not coexist with LSP diagnostics.');

const extension = fs.readFileSync(path.join(root, 'extension.js'), 'utf8');
if (!extension.includes("require('vscode-languageclient/node')")) throw new Error('The extension does not load vscode-languageclient.');
if (!extension.includes("args: ['lsp']")) throw new Error('The extension does not launch `freql lsp`.');
if (extension.includes("scheme: 'untitled'")) throw new Error('The Rust server currently supports file URIs only.');
if (!extension.includes("scheme: 'file'")) throw new Error('The LSP client does not select file-backed freql documents.');
if (!extension.includes('fs.constants.X_OK') || !extension.includes('statSync(candidate).isFile()')) {
  throw new Error('Workspace fallback must verify a regular executable file.');
}
const configuredPath = extension.indexOf("get('server.path', '')");
const debugPath = extension.indexOf("'target', 'debug'");
const pathFallback = extension.indexOf("return 'freql'");
if (configuredPath < 0 || debugPath < 0 || pathFallback < 0 || !(configuredPath < debugPath && debugPath < pathFallback)) {
  throw new Error('Executable resolution must prefer config, then workspace target/debug, then PATH.');
}
for (const duplicatedProvider of [
  'registerCompletionItemProvider',
  'registerHoverProvider',
  'registerSignatureHelpProvider',
  'registerDocumentSymbolProvider',
  'createDiagnosticCollection'
]) {
  if (extension.includes(duplicatedProvider)) throw new Error(`Semantic provider must live in Rust LSP: ${duplicatedProvider}`);
}

const grammar = JSON.parse(fs.readFileSync(path.join(root, 'syntaxes/freql.tmLanguage.json'), 'utf8'));
if (grammar.scopeName !== 'source.freql') throw new Error('Unexpected TextMate scope name.');
for (const word of ['artifact', 'resource', 'process', 'requires', 'gets', 'uses', 'export', 'yield', 'repeat', 'before', 'aggregate', 'monitor']) {
  if (!JSON.stringify(grammar).includes(word)) throw new Error(`Grammar does not mention ${word}.`);
}

console.log('Validated VS Code manifest, language configuration, TextMate grammar, and snippets.');
