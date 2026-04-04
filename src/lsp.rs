//! Thin Language Server Protocol adapter over the compiler-owned IDE queries.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use lsp_server::{Connection, Message, Notification, Request, Response};
use lsp_types::notification::{
    DidChangeTextDocument, DidCloseTextDocument, DidOpenTextDocument,
    Notification as LspNotification, PublishDiagnostics,
};
use lsp_types::request::Request as LspRequest;
use lsp_types::*;
use url::Url;

use crate::diagnostic::Severity as FreqlSeverity;
use crate::{AnalysisHost, AnalysisSnapshot, CompilerOptions, CompletionKind};

pub fn run_stdio() -> Result<(), String> {
    let (connection, io_threads) = Connection::stdio();
    let capabilities = ServerCapabilities {
        hover_provider: Some(HoverProviderCapability::Simple(true)),
        definition_provider: Some(OneOf::Left(true)),
        references_provider: Some(OneOf::Left(true)),
        rename_provider: Some(OneOf::Right(RenameOptions {
            prepare_provider: Some(true),
            work_done_progress_options: Default::default(),
        })),
        document_formatting_provider: Some(OneOf::Left(true)),
        code_action_provider: Some(CodeActionProviderCapability::Simple(true)),
        text_document_sync: Some(TextDocumentSyncCapability::Kind(TextDocumentSyncKind::FULL)),
        completion_provider: Some(CompletionOptions {
            trigger_characters: Some(vec![".".into(), ":".into(), "(".into(), ",".into()]),
            ..CompletionOptions::default()
        }),
        signature_help_provider: Some(SignatureHelpOptions {
            trigger_characters: Some(vec!["(".into(), ",".into()]),
            retrigger_characters: Some(vec![":".into()]),
            work_done_progress_options: WorkDoneProgressOptions::default(),
        }),
        ..ServerCapabilities::default()
    };
    let (initialize_id, initialize_params) = connection
        .initialize_start()
        .map_err(|error| error.to_string())?;
    connection
        .initialize_finish(
            initialize_id,
            serde_json::to_value(InitializeResult {
                capabilities,
                server_info: Some(ServerInfo {
                    name: "freql".into(),
                    version: Some(env!("CARGO_PKG_VERSION").into()),
                }),
            })
            .map_err(|error| error.to_string())?,
        )
        .map_err(|error| error.to_string())?;

    let mut server = Server::new(connection);
    let initialize: InitializeParams =
        serde_json::from_value(initialize_params).map_err(|e| e.to_string())?;
    server.workspace_root = initialize
        .workspace_folders
        .as_ref()
        .and_then(|folders| folders.first())
        .and_then(|folder| uri_to_path(&folder.uri).ok());
    #[allow(deprecated)]
    if server.workspace_root.is_none() {
        server.workspace_root = initialize
            .root_uri
            .as_ref()
            .and_then(|uri| uri_to_path(uri).ok());
    }
    if let Some(options) = initialize.initialization_options {
        server.configure(&options);
    }
    server.run()?;
    // Close the outgoing channel before joining the writer thread; retaining
    // the server here leaves the writer waiting forever after LSP shutdown.
    drop(server);
    io_threads.join().map_err(|error| error.to_string())
}

struct Server {
    connection: Connection,
    host: AnalysisHost,
    open: HashMap<Uri, OpenDocument>,
    published: HashSet<String>,
    workspace_root: Option<PathBuf>,
}

struct OpenDocument {
    path: PathBuf,
    version: i32,
}

impl Server {
    fn new(connection: Connection) -> Self {
        Self {
            connection,
            host: AnalysisHost::new(CompilerOptions {
                absolute_paths: true,
                ..CompilerOptions::default()
            }),
            open: HashMap::new(),
            published: HashSet::new(),
            workspace_root: None,
        }
    }

    fn run(&mut self) -> Result<(), String> {
        while let Ok(message) = self.connection.receiver.recv() {
            match message {
                Message::Request(request) => {
                    if self
                        .connection
                        .handle_shutdown(&request)
                        .map_err(|error| error.to_string())?
                    {
                        return Ok(());
                    }
                    self.handle_request(request)?;
                }
                Message::Notification(notification) => self.handle_notification(notification)?,
                Message::Response(_) => {}
            }
        }
        Ok(())
    }

    fn handle_request(&mut self, request: Request) -> Result<(), String> {
        if matches!(
            request.method.as_str(),
            "textDocument/hover"
                | "textDocument/definition"
                | "textDocument/references"
                | "textDocument/prepareRename"
                | "textDocument/rename"
                | "textDocument/formatting"
                | "textDocument/codeAction"
                | "freql/explainConstraint"
        ) {
            let result = self.editor_request(&request);
            let response = match result {
                Ok(value) => Response::new_ok(request.id, value),
                Err(error) => Response::new_err(
                    request.id,
                    lsp_server::ErrorCode::InvalidRequest as i32,
                    error,
                ),
            };
            return self.send(Message::Response(response));
        }
        if request.method == lsp_types::request::SignatureHelpRequest::METHOD {
            return self.handle_signature_help(request);
        }
        if request.method != lsp_types::request::Completion::METHOD {
            let response = Response::new_err(
                request.id,
                lsp_server::ErrorCode::MethodNotFound as i32,
                format!("unsupported request {}", request.method),
            );
            return self.send(Message::Response(response));
        }
        let params: CompletionParams = match serde_json::from_value(request.params) {
            Ok(params) => params,
            Err(error) => {
                return self.send(Message::Response(Response::new_err(
                    request.id,
                    lsp_server::ErrorCode::InvalidParams as i32,
                    error.to_string(),
                )));
            }
        };
        let uri = params.text_document_position.text_document.uri;
        let Some(document) = self.open.get(&uri) else {
            return self.send(Message::Response(Response::new_ok(
                request.id,
                serde_json::Value::Null,
            )));
        };
        let snapshot = match self.host.snapshot(&document.path) {
            Ok(snapshot) => snapshot,
            Err(_) => {
                return self.send(Message::Response(Response::new_ok(
                    request.id,
                    serde_json::Value::Null,
                )));
            }
        };
        let Some(offset) = offset_for_position(
            &snapshot,
            &document.path,
            params.text_document_position.position,
        ) else {
            return self.send(Message::Response(Response::new_ok(
                request.id,
                serde_json::Value::Null,
            )));
        };
        let items = snapshot
            .completion_at(&document.path, offset)
            .into_iter()
            .map(|candidate| CompletionItem {
                label: candidate.label,
                kind: Some(match candidate.kind {
                    CompletionKind::Field => CompletionItemKind::FIELD,
                    CompletionKind::Configuration | CompletionKind::Parameter => {
                        CompletionItemKind::PROPERTY
                    }
                    CompletionKind::Port => CompletionItemKind::INTERFACE,
                    CompletionKind::Method => CompletionItemKind::METHOD,
                    CompletionKind::Keyword => CompletionItemKind::KEYWORD,
                    CompletionKind::Unit => CompletionItemKind::UNIT,
                }),
                detail: Some(candidate.detail),
                insert_text: Some(candidate.insert_text),
                ..CompletionItem::default()
            })
            .collect::<Vec<_>>();
        let result = CompletionResponse::Array(items);
        self.send(Message::Response(Response::new_ok(
            request.id,
            serde_json::to_value(result).map_err(|error| error.to_string())?,
        )))
    }

    fn handle_signature_help(&mut self, request: Request) -> Result<(), String> {
        let params: SignatureHelpParams = match serde_json::from_value(request.params) {
            Ok(params) => params,
            Err(error) => {
                return self.send(Message::Response(Response::new_err(
                    request.id,
                    lsp_server::ErrorCode::InvalidParams as i32,
                    error.to_string(),
                )));
            }
        };
        let uri = params.text_document_position_params.text_document.uri;
        let Some(document) = self.open.get(&uri) else {
            return self.send(Message::Response(Response::new_ok(
                request.id,
                serde_json::Value::Null,
            )));
        };
        let snapshot = match self.host.snapshot(&document.path) {
            Ok(snapshot) => snapshot,
            Err(_) => {
                return self.send(Message::Response(Response::new_ok(
                    request.id,
                    serde_json::Value::Null,
                )));
            }
        };
        let Some(offset) = offset_for_position(
            &snapshot,
            &document.path,
            params.text_document_position_params.position,
        ) else {
            return self.send(Message::Response(Response::new_ok(
                request.id,
                serde_json::Value::Null,
            )));
        };
        let result = snapshot
            .signature_at(&document.path, offset)
            .map(|candidate| {
                let parameters = candidate
                    .parameters
                    .into_iter()
                    .map(|label| ParameterInformation {
                        label: ParameterLabel::Simple(label),
                        documentation: None,
                    })
                    .collect();
                SignatureHelp {
                    signatures: vec![SignatureInformation {
                        label: candidate.label,
                        documentation: None,
                        parameters: Some(parameters),
                        active_parameter: candidate.active_parameter,
                    }],
                    active_signature: Some(0),
                    active_parameter: candidate.active_parameter,
                }
            });
        self.send(Message::Response(Response::new_ok(
            request.id,
            serde_json::to_value(result).map_err(|error| error.to_string())?,
        )))
    }

    fn handle_notification(&mut self, notification: Notification) -> Result<(), String> {
        match notification.method.as_str() {
            "workspace/didChangeConfiguration" => {
                self.configure(
                    notification
                        .params
                        .get("settings")
                        .unwrap_or(&notification.params),
                );
                self.publish_all()
            }
            DidOpenTextDocument::METHOD => {
                let params: DidOpenTextDocumentParams = serde_json::from_value(notification.params)
                    .map_err(|error| error.to_string())?;
                let path = uri_to_path(&params.text_document.uri)?;
                self.host.set_file(&path, params.text_document.text);
                self.open.insert(
                    params.text_document.uri,
                    OpenDocument {
                        path,
                        version: params.text_document.version,
                    },
                );
                self.publish_all()
            }
            DidChangeTextDocument::METHOD => {
                let params: DidChangeTextDocumentParams =
                    serde_json::from_value(notification.params)
                        .map_err(|error| error.to_string())?;
                let Some(document) = self.open.get_mut(&params.text_document.uri) else {
                    return Ok(());
                };
                if let Some(change) = params.content_changes.into_iter().last() {
                    self.host.set_file(&document.path, change.text);
                    document.version = params.text_document.version;
                }
                self.publish_all()
            }
            DidCloseTextDocument::METHOD => {
                let params: DidCloseTextDocumentParams =
                    serde_json::from_value(notification.params)
                        .map_err(|error| error.to_string())?;
                if let Some(document) = self.open.remove(&params.text_document.uri) {
                    self.host.remove_overlay(&document.path);
                }
                self.published.remove(params.text_document.uri.as_str());
                self.publish(params.text_document.uri, None, Vec::new())?;
                self.publish_all()
            }
            _ => Ok(()),
        }
    }

    fn configure(&mut self, settings: &serde_json::Value) {
        let options = settings.get("freql").unwrap_or(settings);
        let entry = options
            .get("entry")
            .and_then(|value| value.as_str())
            .filter(|s| !s.trim().is_empty())
            .map(PathBuf::from);
        self.host
            .set_entry_model(entry, self.workspace_root.clone());
    }

    fn editor_request(&mut self, request: &Request) -> Result<serde_json::Value, String> {
        use serde_json::json;
        let params = &request.params;
        let uri: Uri = serde_json::from_value(
            params
                .get("textDocument")
                .and_then(|doc| doc.get("uri"))
                .or_else(|| params.get("uri"))
                .cloned()
                .ok_or("Missing document URI")?,
        )
        .map_err(|e| e.to_string())?;
        let document = self
            .open
            .get(&uri)
            .ok_or("Open this document before requesting editor analysis")?;
        let snapshot = self
            .host
            .snapshot(&document.path)
            .map_err(|e| e.to_string())?;
        let position: Position = serde_json::from_value(
            params
                .get("position")
                .cloned()
                .or_else(|| params.get("range").and_then(|r| r.get("start")).cloned())
                .unwrap_or(json!({"line":0,"character":0})),
        )
        .map_err(|e| e.to_string())?;
        let offset = offset_for_position(&snapshot, &document.path, position)
            .ok_or("Position is outside the document")?;
        let convert = |range: &std::ops::Range<usize>| {
            range_for_source(&snapshot, &document.path, range)
                .ok_or_else(|| "Cannot map source span".to_owned())
        };
        let value = match request.method.as_str() {
            "textDocument/hover" => snapshot.hover_at(&document.path, offset).map(|hover| {
                Ok::<_, String>(json!({ "contents": {"kind":"markdown", "value": hover.markdown}, "range": convert(&hover.range)? }))
            }).transpose()?.unwrap_or(serde_json::Value::Null),
            "textDocument/definition" => snapshot.definition_at(&document.path, offset).and_then(|location| location_for_source(&snapshot, &location)).map(|location| json!(location)).unwrap_or(serde_json::Value::Null),
            "textDocument/references" => {
                let include = params.pointer("/context/includeDeclaration").and_then(|v|v.as_bool()).unwrap_or(false);
                json!(snapshot.references_at(&document.path, offset, include).iter().filter_map(|location| location_for_source(&snapshot, location)).collect::<Vec<_>>())
            },
            "textDocument/prepareRename" => {
                let location = snapshot.prepare_rename_at(&document.path, offset)?;
                json!(convert(&location.range)?)
            },
            "textDocument/rename" => {
                let name = params.get("newName").and_then(|v|v.as_str()).ok_or("Missing newName")?;
                let locations = snapshot.rename_at(&document.path, offset, name)?;
                let mut grouped = std::collections::BTreeMap::<String, Vec<TextEdit>>::new();
                for location in &locations {
                    let uri = self.open.iter().find(|(_, doc)| crate::source::source_path(&doc.path).ok().as_ref() == Some(&location.path))
                        .map(|(uri, _)| uri.clone()).unwrap_or(path_to_uri(&location.path)?);
                    let range = range_for_source(&snapshot, &location.path, &location.range).ok_or("Cannot map rename location")?;
                    grouped.entry(uri.to_string()).or_default().push(TextEdit::new(range, name.into()));
                }
                let edits = grouped.into_iter().map(|(uri, edits)| {
                    let parsed = uri.parse::<Uri>().ok();
                    let version = parsed.as_ref().and_then(|uri| self.open.get(uri)).map(|doc| doc.version);
                    json!({"textDocument":{"uri":uri,"version":version},"edits":edits})
                }).collect::<Vec<_>>();
                json!({"documentChanges":edits})
            },
            "textDocument/formatting" => {
                let edits = snapshot.formatting_edits(&document.path)?.into_iter().map(|edit| {
                    Ok(TextEdit::new(convert(&(edit.start..edit.end))?, edit.new_text))
                }).collect::<Result<Vec<_>,String>>()?;
                json!(edits)
            },
            "textDocument/codeAction" => {
                let end: Position = serde_json::from_value(params.pointer("/range/end").cloned().ok_or("Missing range end")?).map_err(|e|e.to_string())?;
                let end = offset_for_position(&snapshot, &document.path, end).ok_or("Invalid range end")?;
                let mut actions = Vec::new();
                for action in snapshot.code_actions_at(&document.path, offset, end) {
                    if let Some(snippet) = action.snippet {
                        actions.push(json!({"title":action.title,"kind":"quickfix","command":{"title":action.title,"command":"freql.insertArgumentPlaceholders","arguments":[{"uri":uri,"range":convert(&(snippet.start..snippet.end))?,"snippet":snippet.new_text,"version":document.version}]}}));
                    } else {
                        let edits = action.edits.into_iter().map(|edit| Ok(TextEdit::new(convert(&(edit.start..edit.end))?, edit.new_text))).collect::<Result<Vec<_>,String>>()?;
                        actions.push(json!({"title":action.title,"kind":"quickfix","edit":{"documentChanges":[{"textDocument":{"uri":uri,"version":document.version},"edits":edits}]}}));
                    }
                }
                // Selections need not start at the diagnostic. Find actual
                // overlapping evidence and pass that origin to the command.
                let canonical_document = crate::source::source_path(&document.path).ok();
                let source = snapshot.sources.files.values().find(|s| s.path == canonical_document);
                if let Some(source) = source {
                    let report = &snapshot.compilation.report;
                    let evidence = report.diagnostics.iter().map(|d| &d.span)
                        .chain(report.constraints.iter().map(|c| &c.span))
                        .chain(report.obligations.iter().map(|o| &o.span))
                        .find(|span| span.path == source.display_path && span.start <= end && offset <= span.end);
                    if let Some(span) = evidence {
                        let range = convert(&(span.start..span.end))?;
                        actions.push(json!({"title":"Explain freql constraint / obligation","command":{"title":"Explain constraint","command":"freql.explainConstraint","arguments":[{"uri":uri,"position":range.start}]}}));
                    }
                }
                json!(actions)
            },
            "freql/explainConstraint" => snapshot.explain_at(&document.path, offset).map(|explanation|json!({"title":explanation.title,"markdown":explanation.markdown})).unwrap_or(serde_json::Value::Null),
            _ => return Err("Unsupported editor query".into()),
        };
        Ok(value)
    }

    fn publish_all(&mut self) -> Result<(), String> {
        let documents = self
            .open
            .iter()
            .map(|(uri, document)| (uri.clone(), document.path.clone(), document.version))
            .collect::<Vec<_>>();
        let mut grouped = HashMap::<String, Vec<Diagnostic>>::new();
        for (uri, path, version) in documents {
            match self.host.snapshot(&path) {
                Ok(snapshot) => {
                    for source in snapshot.sources.files.values() {
                        let Some(source_path) = &source.path else {
                            continue;
                        };
                        let Ok(source_uri) = path_to_uri(source_path) else {
                            continue;
                        };
                        grouped
                            .entry(source_uri.as_str().to_owned())
                            .or_default()
                            .extend(diagnostics_for_source(&snapshot, &source.key));
                    }
                }
                Err(error) => {
                    let diagnostic = Diagnostic {
                        range: Range::default(),
                        severity: Some(DiagnosticSeverity::ERROR),
                        source: Some("freql".into()),
                        message: error.to_string(),
                        ..Diagnostic::default()
                    };
                    grouped
                        .entry(uri.as_str().to_owned())
                        .or_default()
                        .push(diagnostic);
                }
            }
            let _ = version;
        }
        let current = grouped.keys().cloned().collect::<HashSet<_>>();
        for stale in self
            .published
            .difference(&current)
            .cloned()
            .collect::<Vec<_>>()
        {
            if let Ok(uri) = stale.parse::<Uri>() {
                self.publish(uri, None, Vec::new())?;
            }
        }
        self.published = current;
        for (uri, mut diagnostics) in grouped {
            diagnostics.sort_by_key(|diagnostic| {
                (
                    diagnostic.range.start.line,
                    diagnostic.range.start.character,
                    diagnostic.message.clone(),
                )
            });
            diagnostics.dedup_by(|left, right| {
                left.range == right.range
                    && left.code == right.code
                    && left.message == right.message
            });
            let uri = uri.parse::<Uri>().map_err(|error| error.to_string())?;
            let version = self.open.get(&uri).map(|document| document.version);
            self.publish(uri, version, diagnostics)?;
        }
        Ok(())
    }

    fn publish(
        &self,
        uri: Uri,
        version: Option<i32>,
        diagnostics: Vec<Diagnostic>,
    ) -> Result<(), String> {
        let params = PublishDiagnosticsParams::new(uri, diagnostics, version);
        self.send(Message::Notification(Notification::new(
            PublishDiagnostics::METHOD.into(),
            params,
        )))
    }

    fn send(&self, message: Message) -> Result<(), String> {
        self.connection
            .sender
            .send(message)
            .map_err(|error| error.to_string())
    }
}

fn diagnostics_for_source(snapshot: &AnalysisSnapshot, source_key: &str) -> Vec<Diagnostic> {
    let Some(source) = snapshot.sources.files.get(source_key) else {
        return Vec::new();
    };
    let Some(file_id) = snapshot.file_ids.get(&source.key) else {
        return Vec::new();
    };
    let Some(index) = snapshot.line_indexes.get(file_id) else {
        return Vec::new();
    };
    snapshot
        .compilation
        .report
        .diagnostics
        .iter()
        .filter(|diagnostic| diagnostic.span.path == source.display_path)
        .map(|diagnostic| {
            let (start_line, start_character) = index.position(diagnostic.span.start);
            let (end_line, end_character) = index.position(diagnostic.span.end);
            Diagnostic {
                range: Range::new(
                    Position::new(start_line, start_character),
                    Position::new(end_line, end_character),
                ),
                severity: Some(match diagnostic.severity {
                    FreqlSeverity::Error => DiagnosticSeverity::ERROR,
                    FreqlSeverity::Warning => DiagnosticSeverity::WARNING,
                }),
                code: Some(NumberOrString::String(diagnostic.code.clone())),
                source: Some("freql".into()),
                message: snapshot.compilation.display_text(&diagnostic.message),
                data: Some(serde_json::json!({"freqlDiagnosticId":diagnostic.id,"entry":snapshot.compilation.report.entry})),
                ..Diagnostic::default()
            }
        })
        .collect()
}

fn offset_for_position(
    snapshot: &AnalysisSnapshot,
    path: &Path,
    position: Position,
) -> Option<usize> {
    let key = crate::source::source_path(path)
        .ok()?
        .to_string_lossy()
        .into_owned();
    let file_id = snapshot.file_ids.get(&key)?;
    snapshot
        .line_indexes
        .get(file_id)?
        .offset(position.line, position.character)
}

fn range_for_source(
    snapshot: &AnalysisSnapshot,
    path: &Path,
    range: &std::ops::Range<usize>,
) -> Option<Range> {
    let key = crate::source::source_path(path)
        .ok()?
        .to_string_lossy()
        .into_owned();
    let index = snapshot.line_indexes.get(snapshot.file_ids.get(&key)?)?;
    let (line, character) = index.position(range.start);
    let (end_line, end_character) = index.position(range.end);
    Some(Range::new(
        Position::new(line, character),
        Position::new(end_line, end_character),
    ))
}

fn location_for_source(
    snapshot: &AnalysisSnapshot,
    location: &crate::ide_navigation::SourceLocation,
) -> Option<Location> {
    Some(Location::new(
        path_to_uri(&location.path).ok()?,
        range_for_source(snapshot, &location.path, &location.range)?,
    ))
}

fn uri_to_path(uri: &Uri) -> Result<PathBuf, String> {
    Url::parse(uri.as_str())
        .map_err(|error| error.to_string())?
        .to_file_path()
        .map_err(|_| format!("URI is not a local file: {uri:?}"))
}

fn path_to_uri(path: &Path) -> Result<Uri, String> {
    Url::from_file_path(path)
        .map_err(|_| format!("cannot convert path to file URI: {}", path.display()))?
        .as_str()
        .parse::<Uri>()
        .map_err(|error| error.to_string())
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::thread;

    use lsp_server::RequestId;
    use serde_json::json;
    use tempfile::tempdir;

    use super::*;
    use crate::LineIndex;

    #[test]
    fn editor_requests_return_utf16_ranges_versioned_edits_and_constraint_evidence() {
        let directory = tempdir().unwrap();
        let path = directory.path().join("main.req");
        let text = "process P {\nconfig size: Integer = 2\n() => { let caption = \"😀\"\nlet count = size\nrequires count > 0\n}\n}\n";
        fs::write(&path, text).unwrap();
        let uri = path_to_uri(&path).unwrap();
        let (connection, client) = Connection::memory();
        let mut server = Server::new(connection);
        server.host.set_file(&path, text);
        server.open.insert(
            uri.clone(),
            OpenDocument {
                path: path.clone(),
                version: 7,
            },
        );
        let position = |offset| {
            let (line, character) = LineIndex::new(text).position(offset);
            json!({"line":line,"character":character})
        };
        let count = text.find("count =").unwrap();
        let params = json!({"textDocument":{"uri":uri},"position":position(count)});
        for method in [
            "textDocument/hover",
            "textDocument/definition",
            "textDocument/references",
            "textDocument/prepareRename",
        ] {
            server
                .handle_request(Request::new(
                    RequestId::from(1),
                    method.into(),
                    params.clone(),
                ))
                .unwrap();
            let Message::Response(response) = client.receiver.recv().unwrap() else {
                panic!("expected response")
            };
            assert!(response.error.is_none(), "{method}: {:?}", response.error);
            assert!(!response.result.unwrap().is_null(), "{method}");
        }
        let rename = server.editor_request(&Request::new(RequestId::from(2), "textDocument/rename".into(), json!({"textDocument":{"uri":uri},"position":position(count),"newName":"sample_count"}))).unwrap();
        assert_eq!(rename["documentChanges"][0]["textDocument"]["version"], 7);
        assert_eq!(
            rename["documentChanges"][0]["edits"]
                .as_array()
                .unwrap()
                .len(),
            2
        );
        let formatting = server
            .editor_request(&Request::new(
                RequestId::from(3),
                "textDocument/formatting".into(),
                json!({"textDocument":{"uri":uri},"options":{"tabSize":4,"insertSpaces":true}}),
            ))
            .unwrap();
        assert!(formatting.is_array());
        let config = text.find("config size").unwrap();
        let actions = server.editor_request(&Request::new(RequestId::from(4), "textDocument/codeAction".into(), json!({"textDocument":{"uri":uri},"range":{"start":position(config),"end":position(config+6)},"context":{"diagnostics":[]}}))).unwrap();
        assert!(
            actions
                .as_array()
                .unwrap()
                .iter()
                .any(|action| action["title"] == "Expose config size")
        );
        let changed = text.replace("count > 0", "count < 0");
        server.host.set_file(&path, changed.clone());
        let at = position(text.find("count >").unwrap());
        let evidence = server
            .editor_request(&Request::new(
                RequestId::from(5),
                "freql/explainConstraint".into(),
                json!({"uri":uri,"position":at}),
            ))
            .unwrap();
        assert!(evidence["markdown"].as_str().unwrap().contains("Origin:"));
    }

    #[test]
    fn entry_configuration_change_clears_stale_diagnostics_and_missing_entry_is_actionable() {
        let directory = tempdir().unwrap();
        let path = directory.path().join("main.req");
        fs::write(&path, "process P { () => {} }\n").unwrap();
        let uri = path_to_uri(&path).unwrap();
        let (connection, client) = Connection::memory();
        let mut server = Server::new(connection);
        server.workspace_root = Some(directory.path().to_path_buf());
        server.open.insert(
            uri.clone(),
            OpenDocument {
                path: path.clone(),
                version: 2,
            },
        );
        server
            .handle_notification(Notification::new(
                "workspace/didChangeConfiguration".into(),
                json!({"settings":{"freql":{"entry":"missing.req"}}}),
            ))
            .unwrap();
        let Message::Notification(notification) = client.receiver.recv().unwrap() else {
            panic!("expected diagnostics")
        };
        assert!(
            notification.params["diagnostics"][0]["message"]
                .as_str()
                .unwrap()
                .contains("missing.req")
        );
        server
            .handle_notification(Notification::new(
                "workspace/didChangeConfiguration".into(),
                json!({"settings":{"freql":{"entry":null}}}),
            ))
            .unwrap();
        let Message::Notification(notification) = client.receiver.recv().unwrap() else {
            panic!("expected diagnostics")
        };
        assert!(
            notification.params["diagnostics"]
                .as_array()
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn open_completion_shutdown_protocol_round_trip() {
        let directory = tempdir().unwrap();
        let path = directory.path().join("main.req");
        let source = "artifact Sample { mass: Mass }\nprocess Child { expose config amount: Integer = 2 () => {} }\nprocess P { gets sample: Sample () => { let child = Child() requires sample.m > 0 Mass } }\n";
        fs::write(&path, source).unwrap();
        let uri_text = Url::from_file_path(&path).unwrap().to_string();
        let uri: Uri = uri_text.parse().unwrap();
        let (server_connection, client_connection) = Connection::memory();
        let server = thread::spawn(move || Server::new(server_connection).run());

        client_connection
            .sender
            .send(Message::Notification(Notification::new(
                DidOpenTextDocument::METHOD.into(),
                json!({
                    "textDocument": {
                        "uri": uri_text,
                        "languageId": "freql",
                        "version": 1,
                        "text": source
                    }
                }),
            )))
            .unwrap();
        let published = client_connection.receiver.recv().unwrap();
        assert!(
            matches!(published, Message::Notification(ref notification) if notification.method == PublishDiagnostics::METHOD)
        );

        let byte_offset = source.find("sample.m").unwrap() + "sample.m".len();
        let (line, character) = LineIndex::new(source).position(byte_offset);
        client_connection
            .sender
            .send(Message::Request(Request::new(
                RequestId::from(1),
                lsp_types::request::Completion::METHOD.into(),
                json!({
                    "textDocument": { "uri": uri.as_str() },
                    "position": { "line": line, "character": character }
                }),
            )))
            .unwrap();
        let response = client_connection.receiver.recv().unwrap();
        let Message::Response(response) = response else {
            panic!("expected completion response");
        };
        let result = response.result.unwrap();
        let labels = result
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|item| {
                item.get("label")
                    .and_then(|label| label.as_str())
                    .map(str::to_owned)
            })
            .collect::<Vec<_>>();
        assert_eq!(labels, ["mass".to_owned()]);

        let signature_offset = source.find("Child()").unwrap() + "Child(".len();
        let (line, character) = LineIndex::new(source).position(signature_offset);
        client_connection
            .sender
            .send(Message::Request(Request::new(
                RequestId::from(2),
                lsp_types::request::SignatureHelpRequest::METHOD.into(),
                json!({
                    "textDocument": { "uri": uri.as_str() },
                    "position": { "line": line, "character": character }
                }),
            )))
            .unwrap();
        let response = client_connection.receiver.recv().unwrap();
        let Message::Response(response) = response else {
            panic!("expected signature-help response");
        };
        let result = response.result.unwrap();
        assert!(
            result["signatures"][0]["label"]
                .as_str()
                .unwrap()
                .contains("Child configuration(amount: Integer = 2)")
        );

        client_connection
            .sender
            .send(Message::Request(Request::new(
                RequestId::from(3),
                lsp_types::request::Shutdown::METHOD.into(),
                serde_json::Value::Null,
            )))
            .unwrap();
        client_connection
            .sender
            .send(Message::Notification(Notification::new(
                lsp_types::notification::Exit::METHOD.into(),
                serde_json::Value::Null,
            )))
            .unwrap();
        assert!(matches!(
            client_connection.receiver.recv().unwrap(),
            Message::Response(_)
        ));
        assert!(server.join().unwrap().is_ok());
    }

    #[test]
    fn removing_import_clears_published_import_diagnostics() {
        let directory = tempdir().unwrap();
        let entry_path = directory.path().join("main.req");
        let imported_path = directory.path().join("bad.req");
        let original = "import \"bad.req\" (Bad)\nprocess P { () => { requires true } }\n";
        let changed = "process P { () => { requires true } }\n";
        fs::write(&entry_path, original).unwrap();
        fs::write(&imported_path, "artifact Bad { value: MissingType }\n").unwrap();
        let entry_uri = Url::from_file_path(&entry_path).unwrap().to_string();
        let imported_uri = Url::from_file_path(imported_path.canonicalize().unwrap())
            .unwrap()
            .to_string();
        let (server_connection, client_connection) = Connection::memory();
        let server = thread::spawn(move || Server::new(server_connection).run());

        client_connection
            .sender
            .send(Message::Notification(Notification::new(
                DidOpenTextDocument::METHOD.into(),
                json!({
                    "textDocument": {
                        "uri": entry_uri,
                        "languageId": "freql",
                        "version": 1,
                        "text": original
                    }
                }),
            )))
            .unwrap();
        let mut saw_import_error = false;
        let mut observed = Vec::new();
        for _ in 0..2 {
            let Message::Notification(notification) = client_connection.receiver.recv().unwrap()
            else {
                panic!("expected diagnostics notification");
            };
            if notification
                .params
                .get("uri")
                .and_then(|value| value.as_str())
                == Some(imported_uri.as_str())
            {
                saw_import_error = notification
                    .params
                    .get("diagnostics")
                    .and_then(|value| value.as_array())
                    .is_some_and(|items| !items.is_empty());
            }
            observed.push(notification.params);
        }
        assert!(saw_import_error, "observed diagnostics: {observed:#?}");

        client_connection
            .sender
            .send(Message::Notification(Notification::new(
                DidChangeTextDocument::METHOD.into(),
                json!({
                    "textDocument": { "uri": entry_uri, "version": 2 },
                    "contentChanges": [{ "text": changed }]
                }),
            )))
            .unwrap();
        let mut saw_import_clear = false;
        for _ in 0..2 {
            let Message::Notification(notification) = client_connection.receiver.recv().unwrap()
            else {
                panic!("expected diagnostics notification");
            };
            if notification
                .params
                .get("uri")
                .and_then(|value| value.as_str())
                == Some(imported_uri.as_str())
            {
                saw_import_clear = notification
                    .params
                    .get("diagnostics")
                    .and_then(|value| value.as_array())
                    .is_some_and(Vec::is_empty);
            }
        }
        assert!(saw_import_clear);

        client_connection
            .sender
            .send(Message::Request(Request::new(
                RequestId::from(3),
                lsp_types::request::Shutdown::METHOD.into(),
                serde_json::Value::Null,
            )))
            .unwrap();
        client_connection
            .sender
            .send(Message::Notification(Notification::new(
                lsp_types::notification::Exit::METHOD.into(),
                serde_json::Value::Null,
            )))
            .unwrap();
        let _ = client_connection.receiver.recv().unwrap();
        assert!(server.join().unwrap().is_ok());
    }
}
