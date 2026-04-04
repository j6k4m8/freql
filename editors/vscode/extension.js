'use strict';

const path = require('path');
const fs = require('fs');
const { execFile } = require('child_process');
const vscode = require('vscode');
const { LanguageClient } = require('vscode-languageclient/node');

const LANGUAGE_ID = 'freql';
let client;
const explanationDocuments = new Map();

function entryModel(uri) {
  const configured = vscode.workspace.getConfiguration('freql', uri).get('entry', '').trim();
  if (!configured) return null;
  return path.isAbsolute(configured) ? configured : path.resolve(workspaceCwd(uri) || process.cwd(), configured);
}

async function sendConfiguration() {
  if (!client || !client.isRunning()) return;
  const uri = vscode.window.activeTextEditor && vscode.window.activeTextEditor.document.uri;
  await client.sendNotification('workspace/didChangeConfiguration', { settings: { freql: { entry: entryModel(uri) } } });
}

async function explainConstraint(argument) {
  const editor = vscode.window.activeTextEditor;
  if (!client || !client.isRunning()) return;
  const params = argument || (editor && {
    uri: editor.document.uri.toString(),
    position: { line: editor.selection.active.line, character: editor.selection.active.character }
  });
  if (!params) return;
  const explanation = await client.sendRequest('freql/explainConstraint', params);
  if (!explanation) { vscode.window.showInformationMessage('No constraint or obligation at this position.'); return; }
  const uri = vscode.Uri.parse(`freql-explanation:/constraint-${Date.now()}.md`);
  explanationDocuments.set(uri.toString(), `# ${explanation.title}\n\n${explanation.markdown}`);
  await vscode.commands.executeCommand('markdown.showPreview', uri);
}

async function insertArgumentPlaceholders(argument) {
  if (!argument || !argument.uri || !argument.range || typeof argument.snippet !== 'string') return;
  const document = await vscode.workspace.openTextDocument(vscode.Uri.parse(argument.uri));
  if (argument.version !== undefined && document.version !== argument.version) {
    vscode.window.showWarningMessage('The document changed; request the argument fix again.'); return;
  }
  const editor = await vscode.window.showTextDocument(document);
  if (argument.version !== undefined && document.version !== argument.version) {
    vscode.window.showWarningMessage('The document changed; request the argument fix again.'); return;
  }
  const range = new vscode.Range(argument.range.start.line, argument.range.start.character,
    argument.range.end.line, argument.range.end.character);
  await editor.insertSnippet(new vscode.SnippetString(argument.snippet), range);
}

function isRunnableFile(candidate) {
  try {
    if (!fs.statSync(candidate).isFile()) return false;
    if (process.platform !== 'win32') fs.accessSync(candidate, fs.constants.X_OK);
    return true;
  } catch (_) {
    return false;
  }
}

function executableFor(uri) {
  const configured = vscode.workspace.getConfiguration('freql', uri).get('server.path', '').trim();
  if (configured) return configured;

  const folder = uri && vscode.workspace.getWorkspaceFolder(uri);
  const workspaceFolder = folder || (vscode.workspace.workspaceFolders && vscode.workspace.workspaceFolders[0]);
  if (workspaceFolder && workspaceFolder.uri.scheme === 'file') {
    const debugDirectory = path.join(workspaceFolder.uri.fsPath, 'target', 'debug');
    const candidates = process.platform === 'win32'
      ? [path.join(debugDirectory, 'freql.exe'), path.join(debugDirectory, 'freql')]
      : [path.join(debugDirectory, 'freql'), path.join(debugDirectory, 'freql.exe')];
    const local = candidates.find(isRunnableFile);
    if (local) return local;
  }

  return 'freql';
}

function workspaceCwd(uri) {
  const folder = uri && vscode.workspace.getWorkspaceFolder(uri);
  if (folder) return folder.uri.fsPath;
  if (uri && uri.scheme === 'file') return path.dirname(uri.fsPath);
  return vscode.workspace.workspaceFolders && vscode.workspace.workspaceFolders.length
    ? vscode.workspace.workspaceFolders[0].uri.fsPath
    : undefined;
}

function createLanguageClient(output, traceOutput) {
  const activeUri = vscode.window.activeTextEditor && vscode.window.activeTextEditor.document.uri;
  const command = executableFor(activeUri);
  const executable = {
    command,
    args: ['lsp'],
    // Executables default to stdio. Explicit TransportKind.stdio makes the
    // language client append --stdio, which is not a freql CLI argument.
    options: {
      cwd: workspaceCwd(activeUri)
    }
  };
  const serverOptions = {
    run: executable,
    debug: executable
  };
  const clientOptions = {
    documentSelector: [
      { language: LANGUAGE_ID, scheme: 'file' }
    ],
    synchronize: {
      fileEvents: vscode.workspace.createFileSystemWatcher('**/*.req')
    },
    outputChannel: output,
    traceOutputChannel: traceOutput
  };
  return new LanguageClient(
    'freql',
    'freql Language Server',
    serverOptions,
    clientOptions
  );
}

async function startLanguageServer(output, traceOutput, showFailure) {
  if (client) await client.stop().catch(() => {});
  client = createLanguageClient(output, traceOutput);
  try {
    await client.start();
    await sendConfiguration();
  } catch (error) {
    const detail = error && error.message ? error.message : String(error);
    output.appendLine(`[freql] Language server failed to start: ${detail}`);
    output.appendLine('[freql] Verify that freql.server.path points to the current freql CLI, or that `freql lsp` is available on PATH.');
    if (showFailure) {
      vscode.window.showErrorMessage(
        'freql language server could not start. Verify freql.server.path or your PATH.',
        'Show Output'
      ).then(choice => {
        if (choice === 'Show Output') output.show(true);
      });
    }
  }
}

async function checkCurrentFile(output) {
  const document = vscode.window.activeTextEditor && vscode.window.activeTextEditor.document;
  if (!document || document.languageId !== LANGUAGE_ID || document.uri.scheme !== 'file') {
    vscode.window.showWarningMessage('Open a saved .req file before running freql check.');
    return;
  }
  if (document.isDirty && !(await document.save())) {
    vscode.window.showWarningMessage('Save the .req file before running freql check.');
    return;
  }

  const executable = executableFor(document.uri);
  const cwd = workspaceCwd(document.uri);
  execFile(
    executable,
    ['check', entryModel(document.uri) || document.uri.fsPath, '--json'],
    { cwd, maxBuffer: 16 * 1024 * 1024 },
    (error, stdout, stderr) => {
      output.clear();
      output.appendLine(`> ${executable} check ${entryModel(document.uri) || document.uri.fsPath} --json`);
      if (stderr.trim()) output.appendLine(stderr.trim());
      let report;
      try {
        report = JSON.parse(stdout);
        output.appendLine(JSON.stringify(report, null, 2));
      } catch (_) {
        if (stdout.trim()) output.appendLine(stdout.trim());
      }
      output.show(true);
      if (report) {
        vscode.window.showInformationMessage(
          `freql: ${report.status} — ${(report.diagnostics || []).length} diagnostic(s), ${(report.obligations || []).length} obligation(s)`
        );
      } else if (error) {
        vscode.window.showErrorMessage(`freql check failed: ${error.message}`);
      }
    }
  );
}

function activate(context) {
  const output = vscode.window.createOutputChannel('freql');
  const traceOutput = vscode.window.createOutputChannel('freql Language Server Trace');

  context.subscriptions.push(
    output,
    traceOutput,
    vscode.workspace.registerTextDocumentContentProvider('freql-explanation', {
      provideTextDocumentContent: uri => explanationDocuments.get(uri.toString()) || ''
    }),
    vscode.workspace.onDidChangeConfiguration(event => {
      if (event.affectsConfiguration('freql.entry')) void sendConfiguration();
    }),
    vscode.commands.registerCommand('freql.explainConstraint', explainConstraint),
    vscode.commands.registerCommand('freql.insertArgumentPlaceholders', insertArgumentPlaceholders),
    vscode.commands.registerCommand('freql.checkCurrentFile', () => checkCurrentFile(output)),
    vscode.commands.registerCommand('freql.restartLanguageServer', () =>
      startLanguageServer(output, traceOutput, true)
    )
  );

  void startLanguageServer(output, traceOutput, false);
}

async function deactivate() {
  if (client) await client.stop();
}

module.exports = { activate, deactivate };
