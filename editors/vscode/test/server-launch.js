'use strict';

// Requires `cargo build --bin freql`. Exercise the extension's actual launch
// options against the compiler, rather than a separately hardcoded command.
const assert = require('assert');
const fs = require('fs');
const path = require('path');
const vm = require('vm');
const { spawn } = require('child_process');
const { once } = require('events');
const { pathToFileURL } = require('url');
const { createMessageConnection, StreamMessageReader, StreamMessageWriter } = require('vscode-jsonrpc/node');
const repository = path.resolve(__dirname, '../../..');
const binary = process.env.FREQL_TEST_BINARY || path.join(repository, 'target/debug', process.platform === 'win32' ? 'freql.exe' : 'freql');
const vscode = {
  workspace: {
    getConfiguration: () => ({ get: (key, fallback) => key === 'server.path' ? binary : fallback }),
    workspaceFolders: [{ uri: { scheme: 'file', fsPath: repository } }],
    createFileSystemWatcher: () => ({})
  },
  window: {}
};
const context = {
  require: name => name === 'vscode' ? vscode : name === 'vscode-languageclient/node'
    ? { LanguageClient: class {
      constructor(id, name, serverOptions) { this.serverOptions = serverOptions; }
    }, TransportKind: { stdio: 0 } } : require(name),
  module: { exports: {} }, process
};
vm.runInNewContext(fs.readFileSync(path.join(__dirname, '../extension.js'), 'utf8') +
  '\nmodule.exports.launch = createLanguageClient({}, {}).serverOptions.run;', context);
const launch = context.module.exports.launch;
assert.equal(launch.transport, undefined, 'stdio must be implicit to avoid client-injected arguments');

(async () => {
  const child = spawn(launch.command, Array.from(launch.args), { ...launch.options, stdio: 'pipe' });
  const exited = once(child, 'exit');
  let stderr = '';
  child.stderr.on('data', data => { stderr += data; });
  const connection = createMessageConnection(new StreamMessageReader(child.stdout), new StreamMessageWriter(child.stdin));
  const timeout = setTimeout(() => { child.kill(); }, 10000);
  connection.listen();
  try {
    const result = await connection.sendRequest('initialize', { processId: process.pid, rootUri: null, capabilities: {} });
    assert.equal(result.capabilities.hoverProvider, true);
    await connection.sendNotification('initialized', {});
    const uri = pathToFileURL(path.join(repository, 'test/launch-buffer.req')).toString();
    await connection.sendNotification('textDocument/didOpen', {
      textDocument: { uri, languageId: 'freql', version: 1,
        text: 'process P {config x:Scalar=1\n()=>{let y=x+2}}' }
    });
    const edits = await connection.sendRequest('textDocument/formatting', {
      textDocument: { uri }, options: { tabSize: 4, insertSpaces: true }
    });
    assert.equal(edits.length, 1);
    assert.equal(edits[0].newText, 'process P {\n    config x: Scalar = 1\n    () => {\n        let y = x + 2\n    }\n}\n');
    await connection.sendNotification('textDocument/didChange', {
      textDocument: { uri, version: 2 }, contentChanges: [{ text: edits[0].newText }]
    });
    const second = await connection.sendRequest('textDocument/formatting', {
      textDocument: { uri }, options: { tabSize: 4, insertSpaces: true }
    });
    assert.deepEqual(second, [], 'formatting an already formatted buffer should produce no edit');
    await connection.sendRequest('shutdown', null);
    await connection.sendNotification('exit', null);
    child.stdin.end();
    const [code, signal] = await exited;
    assert.equal(code, 0, `Server exited with ${code}/${signal}: ${stderr}`);
    console.log('Extension launch configuration passed a real LSP handshake, unsaved-buffer formatting, and shutdown.');
  } catch (error) {
    throw new Error(`${error.message}\nServer stderr: ${stderr}`, { cause: error });
  } finally {
    clearTimeout(timeout);
    connection.dispose();
    if (child.exitCode === null && child.signalCode === null) child.kill();
  }
})().catch(error => { console.error(error); process.exitCode = 1; });
