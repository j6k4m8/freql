'use strict';

// Exercise the thin client command boundary without launching a VS Code UI.
const assert = require('assert');
const fs = require('fs');
const path = require('path');
const vm = require('vm');
const root = path.resolve(__dirname, '..');
const config = { entry: 'models/main.req' };
const inserted = [];
const warnings = [];
const document = { version: 7 };
const vscode = {
  workspace: {
    getConfiguration: () => ({ get: (name, fallback) => config[name] ?? fallback }),
    getWorkspaceFolder: () => ({ uri: { fsPath: '/workspace' } }),
    openTextDocument: async () => document
  },
  window: {
    showWarningMessage: message => warnings.push(message),
    showTextDocument: async () => ({ insertSnippet: (snippet, range) => inserted.push({ snippet, range }) })
  },
  Uri: { parse: value => value },
  Range: class { constructor(...args) { this.args = args; } },
  SnippetString: class { constructor(value) { this.value = value; } }
};
const context = {
  require: name => name === 'vscode' ? vscode : name === 'vscode-languageclient/node'
    ? { LanguageClient: class {
      constructor(id, name, serverOptions) { this.serverOptions = serverOptions; }
    }, TransportKind: { stdio: 0 } } : require(name),
  module: { exports: {} }, process,
};
vm.runInNewContext(fs.readFileSync(path.join(root, 'extension.js'), 'utf8') +
  '\nmodule.exports.test = { entryModel, insertArgumentPlaceholders, createLanguageClient };', context);
const { entryModel, insertArgumentPlaceholders, createLanguageClient } = context.module.exports.test;

(async () => {
  config['server.path'] = '/compiler/freql';
  vscode.workspace.createFileSystemWatcher = () => ({});
  const server = createLanguageClient({}, {}).serverOptions;
  assert.equal(server.run.command, '/compiler/freql');
  assert.deepEqual(Array.from(server.run.args), ['lsp']);
  assert.equal(server.run.transport, undefined, 'explicit stdio injects an unsupported --stdio flag');
  assert.strictEqual(server.run, server.debug);
  assert.equal(entryModel('file:///workspace/a.req'), '/workspace/models/main.req');
  config.entry = '/other/main.req';
  assert.equal(entryModel('file:///workspace/a.req'), '/other/main.req');
  config.entry = '';
  assert.equal(entryModel('file:///workspace/a.req'), null);
  const argument = { uri: 'file:///workspace/a.req', version: 7,
    range: { start: { line: 3, character: 5 }, end: { line: 3, character: 5 } },
    snippet: 'size: ${1:TODO_size}' };
  await insertArgumentPlaceholders(argument);
  assert.equal(inserted.length, 1);
  assert.equal(inserted[0].snippet.value, 'size: ${1:TODO_size}');
  document.version = 8;
  await insertArgumentPlaceholders(argument);
  assert.equal(inserted.length, 1, 'stale snippet must not edit a changed document');
  assert.equal(warnings.length, 1);
  console.log('Validated entry paths and version-guarded snippet commands.');
})().catch(error => { console.error(error); process.exitCode = 1; });
