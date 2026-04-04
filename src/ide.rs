//! Stable, query-oriented compiler services used by editor integrations.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use indexmap::IndexMap;
use serde::Serialize;

use crate::analyzer::{Compilation, CompileError, Compiler, CompilerOptions, ProcessModel};
use crate::ast::{Item, MemberRole};
use crate::source::{SourceSet, load_with_overlays};
use crate::types::Type;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize)]
pub struct FileId(pub u32);

#[derive(Clone, Debug)]
pub struct LineIndex {
    text: String,
    line_starts: Vec<usize>,
}

impl LineIndex {
    pub fn new(text: impl Into<String>) -> Self {
        let text = text.into();
        let mut line_starts = vec![0];
        for (offset, byte) in text.bytes().enumerate() {
            if byte == b'\n' {
                line_starts.push(offset + 1);
            }
        }
        Self { text, line_starts }
    }

    pub fn position(&self, offset: usize) -> (u32, u32) {
        let offset = offset.min(self.text.len());
        let line = self
            .line_starts
            .partition_point(|start| *start <= offset)
            .saturating_sub(1);
        let start = self.line_starts[line];
        let character = self.text[start..offset].encode_utf16().count();
        (line as u32, character as u32)
    }

    pub fn offset(&self, line: u32, character: u32) -> Option<usize> {
        let start = *self.line_starts.get(line as usize)?;
        let end = self
            .line_starts
            .get(line as usize + 1)
            .copied()
            .unwrap_or(self.text.len());
        let mut utf16 = 0_u32;
        for (relative, value) in self.text[start..end].char_indices() {
            if utf16 == character {
                return Some(start + relative);
            }
            utf16 += value.len_utf16() as u32;
            if utf16 > character {
                return None;
            }
        }
        (utf16 == character).then_some(end)
    }
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CompletionKind {
    Field,
    Configuration,
    Parameter,
    Port,
    Method,
    Keyword,
    Unit,
}

#[derive(Clone, Debug, Serialize)]
pub struct CompletionCandidate {
    pub label: String,
    pub kind: CompletionKind,
    pub detail: String,
    pub insert_text: String,
    pub definition: Option<String>,
}

#[derive(Clone, Debug, Serialize)]
pub struct SignatureCandidate {
    pub label: String,
    pub parameters: Vec<String>,
    pub active_parameter: Option<u32>,
}

#[derive(Clone, Debug)]
pub struct AnalysisSnapshot {
    pub compilation: Compilation,
    pub sources: SourceSet,
    pub file_ids: IndexMap<String, FileId>,
    pub line_indexes: IndexMap<FileId, LineIndex>,
    pub entry_context: EntryContext,
}

/// The entry used for this query, and whether a workspace selection applies.
#[derive(Clone, Debug, Serialize)]
pub struct EntryContext {
    pub configured_entry: Option<PathBuf>,
    pub analysis_entry: PathBuf,
    pub uses_configured_entry: bool,
}

impl AnalysisSnapshot {
    pub fn completion_at(&self, path: &Path, offset: usize) -> Vec<CompletionCandidate> {
        let Ok(canonical) = crate::source::source_path(path) else {
            return Vec::new();
        };
        let key = canonical.to_string_lossy();
        let Some(source) = self.sources.files.get(key.as_ref()) else {
            return Vec::new();
        };
        if crate::lexer::lex(&source.text).tokens.iter().any(|token|
            token.span.start <= offset && offset < token.span.end && matches!(token.kind,
                crate::syntax::SyntaxKind::Comment | crate::syntax::SyntaxKind::String | crate::syntax::SyntaxKind::DocString)
        ) { return Vec::new(); }
        if let Some(candidates) = self.extended_completion_at(path, offset) {
            return candidates;
        }
        let process = source
            .parse
            .module
            .items
            .iter()
            .find_map(|item| match item {
                Item::Process(process)
                    if process.span.start <= offset && offset <= process.span.end =>
                {
                    self.compilation.model.processes.values().find(|model| model.path == format!("{}::{}", source.display_path, process.name))
                        .or_else(|| self.compilation.model.processes.get(&process.name))
                }
                _ => None,
            });
        let Some(process) = process else {
            return Vec::new();
        };
        if let Some(context) = monitor_context(&source.text, offset) {
            return self.monitor_candidates(source, process, context);
        }
        if let Some((receiver, prefix)) = member_context(&source.text, offset) {
            return self.member_candidates(source, process, receiver, prefix, offset);
        }
        if let Some((callee, prefix, supplied, chained_invocation)) =
            call_context(&source.text, offset)
        {
            return self.call_candidates(
                source,
                process,
                callee,
                prefix,
                &supplied,
                offset,
                chained_invocation,
            );
        }
        if let Some(prefix) = repeat_block_context(&source.text, offset) {
            let mut candidates = process
                .bindings
                .iter()
                .filter(|(name, _)| {
                    name.starts_with(prefix)
                        && process
                            .binding_declarations
                            .get(*name)
                            .is_none_or(|declaration| *declaration <= offset)
                })
                .map(|(name, ty)| {
                    let (kind, detail) = match ty {
                        Type::ConfiguredProcess(target) => (
                            CompletionKind::Configuration,
                            format!("configured process: {target}"),
                        ),
                        Type::Run(target) => {
                            (CompletionKind::Port, format!("process run: {target}"))
                        }
                        _ => (CompletionKind::Parameter, ty.to_string()),
                    };
                    CompletionCandidate {
                        label: name.clone(),
                        kind,
                        detail,
                        insert_text: name.clone(),
                        definition: None,
                    }
                })
                .collect::<Vec<_>>();
            candidates.sort_by(|left, right| left.label.cmp(&right.label));
            return candidates;
        }
        Vec::new()
    }

    pub fn signature_at(&self, path: &Path, offset: usize) -> Option<SignatureCandidate> {
        let canonical = crate::source::source_path(path).ok()?;
        let key = canonical.to_string_lossy();
        let source = self.sources.files.get(key.as_ref())?;
        let process = source
            .parse
            .module
            .items
            .iter()
            .find_map(|item| match item {
                Item::Process(process) if process.span.contains(&offset) => {
                    self.compilation.model.processes.values().find(|model| model.path == format!("{}::{}", source.display_path, process.name))
                        .or_else(|| self.compilation.model.processes.get(&process.name))
                }
                _ => None,
            })?;
        let (callee, prefix, supplied, chained_invocation) = call_context(&source.text, offset)?;

        let resolved = self.resolve_visible_name(source, callee);
        let (phase, target) = match process.bindings.get(callee) {
            Some(Type::ConfiguredProcess(name)) => {
                ("invocation", self.compilation.model.processes.get(name)?)
            }
            _ if chained_invocation && resolved.is_some() => {
                ("invocation", self.compilation.model.processes.get(resolved.as_deref()?)?)
            }
            _ if resolved.is_some() => (
                "configuration",
                self.compilation.model.processes.get(resolved.as_deref()?)?,
            ),
            _ => return None,
        };
        let parameters = if phase == "configuration" {
            target
                .members
                .iter()
                .filter(|(_, member)| member.role == MemberRole::Config)
                .map(|(name, member)| {
                    member.default.as_ref().map_or_else(
                        || format!("{name}: {}", member.ty),
                        |default| format!("{name}: {} = {default}", member.ty),
                    )
                })
                .collect::<Vec<_>>()
        } else {
            target
                .parameters
                .iter()
                .map(|(name, ty)| format!("{name}: {ty}"))
                .collect::<Vec<_>>()
        };
        let names = parameters
            .iter()
            .map(|parameter| parameter.split(':').next().unwrap_or_default())
            .collect::<Vec<_>>();
        let active_parameter = names
            .iter()
            .position(|name| !prefix.is_empty() && name.starts_with(prefix))
            .or_else(|| {
                names
                    .iter()
                    .position(|name| !supplied.iter().any(|item| item == name))
            })
            .map(|index| index as u32);
        Some(SignatureCandidate {
            label: format!("{callee} {phase}({})", parameters.join(", ")),
            parameters,
            active_parameter,
        })
    }

    fn monitor_candidates(
        &self,
        source: &crate::source::SourceFile,
        process: &ProcessModel,
        context: MonitorCompletionContext<'_>,
    ) -> Vec<CompletionCandidate> {
        match context {
            MonitorCompletionContext::Metric(prefix) => process
                .members
                .iter()
                .filter(|(name, member)| {
                    name.starts_with(prefix)
                        && member.role == MemberRole::Export
                        && member.aggregate.is_some()
                })
                .map(|(name, member)| CompletionCandidate {
                    label: name.clone(),
                    kind: CompletionKind::Port,
                    detail: format!("monitored aggregate: {}", member.ty),
                    insert_text: name.clone(),
                    definition: Some(member.declared_by.clone()),
                })
                .collect(),
            MonitorCompletionContext::As(prefix) => "as"
                .starts_with(prefix)
                .then(|| CompletionCandidate {
                    label: "as".into(),
                    kind: CompletionKind::Keyword,
                    detail: "display the monitored value in a compatible unit".into(),
                    insert_text: "as ".into(),
                    definition: None,
                })
                .into_iter()
                .collect(),
            MonitorCompletionContext::Unit(prefix) => {
                const BUILTIN_UNITS: &[&str] = &[
                    "Scalar",
                    "Length",
                    "Meter",
                    "Mass",
                    "Kilogram",
                    "Time",
                    "Second",
                    "Current",
                    "Ampere",
                    "Temperature",
                    "Kelvin",
                    "Amount",
                    "Mole",
                    "LuminousIntensity",
                    "Candela",
                    "USD",
                    "Area",
                    "Volume",
                ];
                let mut units = BUILTIN_UNITS
                    .iter()
                    .map(|name| ((*name).to_owned(), "built-in unit".to_owned(), None))
                    .chain(
                        self.compilation
                            .model
                            .units
                            .iter()
                            .filter(|(name, _)| visible_in(source, name))
                            .map(|(name, unit)| {
                                (
                                    name.clone(),
                                    format!("{}; scale {}", unit.dimension, unit.scale),
                                    Some(unit.path.clone()),
                                )
                            }),
                    )
                    .filter(|(name, _, _)| name.starts_with(prefix))
                    .map(|(name, detail, definition)| CompletionCandidate {
                        label: name.clone(),
                        kind: CompletionKind::Unit,
                        detail,
                        insert_text: name,
                        definition,
                    })
                    .collect::<Vec<_>>();
                units.sort_by(|left, right| left.label.cmp(&right.label));
                units
            }
        }
    }

    fn member_candidates(
        &self,
        source: &crate::source::SourceFile,
        process: &ProcessModel,
        receiver: &str,
        prefix: &str,
        offset: usize,
    ) -> Vec<CompletionCandidate> {
        let receiver = source.parse.module.items.iter().find_map(|item| match item {
            Item::Import(import) if import.alias.as_deref() == Some(receiver) => match &import.path {
                crate::ast::ImportPath::Module(module) if module == "math" || module == "std.stream" => Some(module.as_str()),
                _ => None,
            },
            _ => None,
        }).unwrap_or(receiver);
        if receiver == "math" && imports_module(source, "math") {
            return [
                ("ceil", "ceil(value: numeric) -> same unit or Integer"),
                ("floor", "floor(value: numeric) -> same unit or Integer"),
                ("range", "range(start: Integer, end: Integer) -> Integer[]"),
                ("sqrt", "sqrt(value: numeric) -> numeric"),
                ("sum", "sum(values: numeric[]) -> numeric"),
            ]
            .into_iter()
            .filter(|(name, _)| name.starts_with(prefix))
            .map(|(name, detail)| CompletionCandidate {
                label: name.into(),
                kind: CompletionKind::Method,
                detail: detail.into(),
                insert_text: format!("{name}()"),
                definition: Some("<native:math>".into()),
            })
            .collect();
        }
        if matches!(receiver, "stream" | "std.stream") && imports_module(source, "std.stream") {
            return [
                (
                    "zip",
                    "zip(left: Stream[A], right: Stream[B]) -> Stream[(left: A, right: B)]",
                ),
                (
                    "zip_exact",
                    "zip_exact(left: Stream[A], right: Stream[B]) -> Stream[(left: A, right: B)]",
                ),
            ]
            .into_iter()
            .filter(|(name, _)| name.starts_with(prefix))
            .map(|(name, detail)| CompletionCandidate {
                label: name.into(),
                kind: CompletionKind::Method,
                detail: detail.into(),
                insert_text: format!("{name}()"),
                definition: Some("<std/stream>".into()),
            })
            .collect();
        }
        if process
            .binding_declarations
            .get(receiver)
            .is_some_and(|declared_at| *declared_at > offset)
        {
            return Vec::new();
        }
        let mut parts = receiver.split('.');
        let Some(mut ty) = parts.next().and_then(|name| process.bindings.get(name)) else {
            return Vec::new();
        };
        for field in parts {
            ty = match ty {
                Type::NamedTuple(fields) => fields.iter().find(|(name, _)| name == field).map(|(_, ty)| ty),
                Type::Artifact(name) => self
                    .compilation
                    .model
                    .artifacts
                    .get(name)
                    .and_then(|a| a.fields.get(field)),
                Type::Run(name) => self
                    .compilation
                    .model
                    .processes
                    .get(name)
                    .and_then(|p| p.members.get(field))
                    .map(|m| &m.ty),
                _ => None,
            }
            .unwrap_or(&Type::Unknown);
        }
        match ty {
            Type::Artifact(name) => self
                .compilation
                .model
                .artifacts
                .get(name)
                .map(|artifact| {
                    artifact
                        .fields
                        .iter()
                        .filter(|(name, _)| name.starts_with(prefix))
                        .map(|(name, ty)| CompletionCandidate {
                            label: name.clone(),
                            kind: CompletionKind::Field,
                            detail: if artifact.derived_fields.contains(name) {
                                format!("{ty} (derived, read-only)")
                            } else {
                                ty.to_string()
                            },
                            insert_text: name.clone(),
                            definition: Some(artifact.path.clone()),
                        })
                        .collect()
                })
                .unwrap_or_default(),
            Type::Run(name) => self
                .compilation
                .model
                .processes
                .get(name)
                .map(|run| {
                    member_completions(run, prefix, None)
                        .into_iter()
                        .filter(|candidate| {
                            !matches!(candidate.kind, CompletionKind::Configuration)
                        })
                        .collect()
                })
                .unwrap_or_default(),
            Type::Array { .. } => ["length", "append"]
                .into_iter()
                .filter(|name| name.starts_with(prefix))
                .map(|name| CompletionCandidate {
                    label: name.into(),
                    kind: CompletionKind::Method,
                    detail: "array method".into(),
                    insert_text: format!("{name}()"),
                    definition: None,
                })
                .collect(),
            Type::NamedTuple(fields) => fields
                .iter()
                .filter(|(name, _)| name.starts_with(prefix))
                .map(|(name, ty)| CompletionCandidate {
                    label: name.clone(),
                    kind: CompletionKind::Field,
                    detail: ty.to_string(),
                    insert_text: name.clone(),
                    definition: None,
                })
                .collect(),
            _ => Vec::new(),
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn call_candidates(
        &self,
        source: &crate::source::SourceFile,
        process: &ProcessModel,
        callee: &str,
        prefix: &str,
        supplied: &[String],
        offset: usize,
        chained_invocation: bool,
    ) -> Vec<CompletionCandidate> {
        let local_is_visible = process
            .binding_declarations
            .get(callee)
            .is_none_or(|declared_at| *declared_at <= offset);
        let resolved = self.resolve_visible_name(source, callee);
        let candidates = match process.bindings.get(callee).filter(|_| local_is_visible) {
            Some(Type::ConfiguredProcess(name)) => self
                .compilation
                .model
                .processes
                .get(name)
                .map(|target| {
                    target
                        .parameters
                        .iter()
                        .map(|(name, ty)| CompletionCandidate {
                            label: name.clone(),
                            kind: CompletionKind::Parameter,
                            detail: ty.to_string(),
                            insert_text: format!("{name}: "),
                            definition: Some(target.path.clone()),
                        })
                        .collect::<Vec<_>>()
                })
                .unwrap_or_default(),
            _ if chained_invocation && resolved.is_some() => self
                .compilation
                .model
                .processes
                .get(resolved.as_deref().unwrap())
                .map(|target| {
                    target
                        .parameters
                        .iter()
                        .map(|(name, ty)| CompletionCandidate {
                            label: name.clone(),
                            kind: CompletionKind::Parameter,
                            detail: ty.to_string(),
                            insert_text: format!("{name}: "),
                            definition: Some(target.path.clone()),
                        })
                        .collect::<Vec<_>>()
                })
                .unwrap_or_default(),
            _ if resolved.is_some() => self
                .compilation
                .model
                .processes
                .get(resolved.as_deref().unwrap())
                .map(|target| member_completions(target, "", Some(MemberRole::Config)))
                .or_else(|| {
                    self.compilation
                        .model
                        .artifacts
                        .get(resolved.as_deref().unwrap())
                        .map(|artifact| {
                            artifact
                                .fields
                                .iter()
                                .filter(|(name, _)| !artifact.derived_fields.contains(*name))
                                .map(|(name, ty)| CompletionCandidate {
                                    label: name.clone(),
                                    kind: CompletionKind::Field,
                                    detail: ty.to_string(),
                                    insert_text: format!("{name}: "),
                                    definition: Some(artifact.path.clone()),
                                })
                                .collect()
                        })
                })
                .or_else(|| self.compilation.model.functions.get(resolved.as_deref().unwrap()).map(|function|
                    function.parameters.iter().map(|(name, ty)| CompletionCandidate {
                        label: name.clone(), kind: CompletionKind::Parameter, detail: ty.to_string(),
                        insert_text: format!("{name}: "), definition: Some(function.path.clone()),
                    }).collect()
                ))
                .unwrap_or_default(),
            _ => Vec::new(),
        };
        candidates
            .into_iter()
            .filter(|candidate| {
                candidate.label.starts_with(prefix) && !supplied.contains(&candidate.label)
            })
            .map(|mut candidate| {
                if matches!(
                    candidate.kind,
                    CompletionKind::Configuration | CompletionKind::Parameter
                ) && process.bindings.contains_key(&candidate.label)
                {
                    candidate.insert_text = candidate.label.clone();
                    candidate.detail.push_str(" · same-name shorthand");
                }
                candidate
            })
            .collect()
    }
}

fn visible_in(source: &crate::source::SourceFile, name: &str) -> bool {
    source.parse.module.items.iter().any(|item| match item {
        Item::Import(import) => import.names.iter().any(|imported| imported == name),
        item => item.name() == Some(name),
    })
}

fn imports_module(source: &crate::source::SourceFile, module: &str) -> bool {
    source.parse.module.items.iter().any(|item| {
        matches!(
            item,
            Item::Import(import)
                if matches!(&import.path, crate::ast::ImportPath::Module(name) if name == module)
        )
    })
}

#[derive(Clone, Debug)]
pub struct AnalysisHost {
    options: CompilerOptions,
    overlays: HashMap<PathBuf, String>,
    ids: IndexMap<PathBuf, FileId>,
    next_id: u32,
    entry_model: Option<PathBuf>,
    workspace_root: Option<PathBuf>,
}

impl AnalysisHost {
    pub fn new(options: CompilerOptions) -> Self {
        Self {
            options,
            overlays: HashMap::new(),
            ids: IndexMap::new(),
            next_id: 0,
            entry_model: None,
            workspace_root: None,
        }
    }

    /// Set the optional workspace entry. Errors are reported by `snapshot` so
    /// an invalid setting never silently falls back to a different model.
    pub fn set_entry_model(&mut self, entry: Option<PathBuf>, workspace_root: Option<PathBuf>) {
        self.entry_model = entry.filter(|path| !path.as_os_str().is_empty());
        self.workspace_root = workspace_root;
    }

    fn configured_entry(&self) -> Result<Option<PathBuf>, CompileError> {
        let Some(entry) = &self.entry_model else { return Ok(None) };
        let path = if entry.is_absolute() { entry.clone() } else {
            let root = self.workspace_root.as_ref().ok_or_else(|| CompileError::EntryModel(
                format!("freql.entry is relative ({}) but no workspace folder is available; open a workspace or use an absolute entry path", entry.display())
            ))?;
            root.join(entry)
        };
        Ok(Some(canonical_or_absolute(path)))
    }

    pub fn set_file(&mut self, path: impl Into<PathBuf>, text: impl Into<String>) -> FileId {
        let path = canonical_or_absolute(path.into());
        let id = *self.ids.entry(path.clone()).or_insert_with(|| {
            let id = FileId(self.next_id);
            self.next_id += 1;
            id
        });
        self.overlays.insert(path, text.into());
        id
    }

    pub fn remove_overlay(&mut self, path: &Path) {
        self.overlays
            .remove(&canonical_or_absolute(path.to_path_buf()));
    }

    pub fn snapshot(&mut self, entry: &Path) -> Result<AnalysisSnapshot, CompileError> {
        let document = canonical_or_absolute(entry.to_path_buf());
        let configured_entry = self.configured_entry()?;
        let (sources, analysis_entry, uses_configured_entry) = if let Some(selected) = &configured_entry {
            let sources = load_with_overlays(selected, self.options.absolute_paths, &self.overlays)
                .map_err(|error| CompileError::EntryModel(format!(
                    "cannot analyze configured freql.entry {}: {error}; correct freql.entry or clear it to analyze each file independently", selected.display()
                )))?;
            if sources.files.values().any(|source| source.path.as_ref() == Some(&document)) {
                (sources, selected.clone(), true)
            } else {
                (load_with_overlays(&document, self.options.absolute_paths, &self.overlays)?, document.clone(), false)
            }
        } else {
            (load_with_overlays(&document, self.options.absolute_paths, &self.overlays)?, document.clone(), false)
        };
        let mut file_ids = IndexMap::new();
        let mut line_indexes = IndexMap::new();
        for (key, source) in &sources.files {
            let path = source.path.clone().unwrap_or_else(|| PathBuf::from(key));
            let id = *self.ids.entry(path).or_insert_with(|| {
                let id = FileId(self.next_id);
                self.next_id += 1;
                id
            });
            file_ids.insert(key.clone(), id);
            line_indexes.insert(id, LineIndex::new(source.text.clone()));
        }
        let compilation = Compiler::new(self.options.clone()).check_sources(sources.clone())?;
        Ok(AnalysisSnapshot {
            compilation,
            sources,
            file_ids,
            line_indexes,
            entry_context: EntryContext { configured_entry, analysis_entry, uses_configured_entry },
        })
    }
}

fn member_completions(
    process: &ProcessModel,
    prefix: &str,
    role: Option<MemberRole>,
) -> Vec<CompletionCandidate> {
    process
        .members
        .iter()
        .filter(|(name, member)| {
            name.starts_with(prefix) && role.is_none_or(|role| member.role == role)
        })
        .map(|(name, member)| {
            let role_label = match member.role {
                MemberRole::Config => "configuration",
                MemberRole::Gets => "gets port",
                MemberRole::Uses => "uses port",
                MemberRole::Export => "export port",
            };
            let qualifier = if member.exposed { "exposed " } else { "" };
            CompletionCandidate {
                label: name.clone(),
                kind: if member.role == MemberRole::Config {
                    CompletionKind::Configuration
                } else {
                    CompletionKind::Port
                },
                detail: member.default.as_ref().map_or_else(
                    || format!("{qualifier}{role_label}: {}", member.ty),
                    |default| {
                        format!(
                            "{qualifier}{role_label}: {} · default = {default}",
                            member.ty
                        )
                    },
                ),
                insert_text: if role == Some(MemberRole::Config) {
                    format!("{name}: ")
                } else {
                    name.clone()
                },
                definition: Some(member.declared_by.clone()),
            }
        })
        .collect()
}

fn member_context(text: &str, offset: usize) -> Option<(&str, &str)> {
    let before = text.get(..offset)?;
    let prefix_start = before
        .char_indices()
        .rev()
        .take_while(|(_, value)| value.is_ascii_alphanumeric() || *value == '_')
        .last()
        .map(|(index, _)| index)
        .unwrap_or(before.len());
    let prefix = &before[prefix_start..];
    let dot = prefix_start.checked_sub(1)?;
    if before.as_bytes().get(dot) != Some(&b'.') {
        return None;
    }
    let receiver_end = dot;
    let receiver_start = before[..receiver_end]
        .char_indices()
        .rev()
        .take_while(|(_, value)| value.is_ascii_alphanumeric() || *value == '_' || *value == '.')
        .last()
        .map(|(index, _)| index)
        .unwrap_or(receiver_end);
    (receiver_start < receiver_end).then_some((&before[receiver_start..receiver_end], prefix))
}

fn repeat_block_context(text: &str, offset: usize) -> Option<&str> {
    let before = text.get(..offset)?;
    let line_start = before.rfind('\n').map_or(0, |index| index + 1);
    let prefix = before[line_start..].trim();
    if !prefix
        .chars()
        .all(|character| character.is_ascii_alphanumeric() || character == '_')
    {
        return None;
    }
    let mut depth = 0_i32;
    for (index, character) in before[..line_start].char_indices().rev() {
        match character {
            '}' => depth += 1,
            '{' if depth > 0 => depth -= 1,
            '{' => {
                let header_start = before[..index].rfind(['\n', '}']).map_or(0, |at| at + 1);
                let header = before[header_start..index].trim();
                return header.starts_with("repeat ").then_some(prefix);
            }
            _ => {}
        }
    }
    None
}

#[derive(Clone, Copy, Debug)]
enum MonitorCompletionContext<'a> {
    Metric(&'a str),
    As(&'a str),
    Unit(&'a str),
}

fn monitor_context(text: &str, offset: usize) -> Option<MonitorCompletionContext<'_>> {
    let before = text.get(..offset)?;
    let line = before.rsplit_once('\n').map_or(before, |(_, line)| line);
    let line = line.split('#').next()?.trim_start();
    let rest = line.strip_prefix("monitor")?;
    if rest.starts_with(|value: char| value.is_ascii_alphanumeric() || value == '_') {
        return None;
    }
    let rest = rest.trim_start();
    if rest.is_empty() {
        return Some(MonitorCompletionContext::Metric(""));
    }
    let words = rest.split_whitespace().collect::<Vec<_>>();
    match words.as_slice() {
        [metric] if rest.ends_with(char::is_whitespace) => {
            let _ = metric;
            Some(MonitorCompletionContext::As(""))
        }
        [metric] => Some(MonitorCompletionContext::Metric(metric)),
        [_, keyword] if *keyword != "as" || !rest.ends_with(char::is_whitespace) => {
            Some(MonitorCompletionContext::As(keyword))
        }
        [_, "as"] => Some(MonitorCompletionContext::Unit("")),
        [_, "as", unit, ..] => Some(MonitorCompletionContext::Unit(unit)),
        _ => None,
    }
}

fn call_context(text: &str, offset: usize) -> Option<(&str, &str, Vec<String>, bool)> {
    let before = text.get(..offset)?;
    use crate::syntax::SyntaxKind;
    let tokens = crate::lexer::lex(before).tokens;
    let mut stack = Vec::new();
    for token in &tokens {
        match token.kind {
            SyntaxKind::LParen => stack.push(token.span.start),
            SyntaxKind::RParen => { stack.pop(); },
            _ => {}
        }
    }
    let open = stack.pop()?;
    let mut callee_end = open;
    let chained_invocation = before[..open].trim_end().ends_with(')');
    if chained_invocation {
        let close = before[..open].trim_end().len().checked_sub(1)?;
        let mut depth = 0_usize;
        let mut matching_open = None;
        for (index, value) in before[..=close].char_indices().rev() {
            match value {
                ')' => depth += 1,
                '(' => {
                    depth = depth.checked_sub(1)?;
                    if depth == 0 {
                        matching_open = Some(index);
                        break;
                    }
                }
                _ => {}
            }
        }
        callee_end = matching_open?;
    }
    let callee_start = before[..callee_end]
        .char_indices()
        .rev()
        .take_while(|(_, value)| value.is_ascii_alphanumeric() || *value == '_' || *value == '.')
        .last()
        .map(|(index, _)| index)
        .unwrap_or(callee_end);
    if callee_start == callee_end {
        return None;
    }
    let mut depth = 0_i32;
    let mut segment_start = open + 1;
    let mut segments = Vec::new();
    for token in tokens.iter().filter(|t| t.span.start > open) {
        match token.kind {
            SyntaxKind::LParen | SyntaxKind::LBracket | SyntaxKind::LBrace => depth += 1,
            SyntaxKind::RParen | SyntaxKind::RBracket | SyntaxKind::RBrace => depth -= 1,
            SyntaxKind::Comma if depth == 0 => {
                segments.push(&before[segment_start..token.span.start]);
                segment_start = token.span.end;
            },
            _ => {}
        }
    }
    let current = before[segment_start..].trim_start();
    let mut prefix = current
        .split_once(':')
        .map(|_| "")
        .unwrap_or_else(|| current.trim());
    let mut supplied = segments.into_iter().filter_map(|argument| {
        let name = argument.split_once(':').map_or(argument.trim(), |(name, _)| name.trim());
        (!name.is_empty() && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')).then(|| name.to_owned())
    }).collect::<Vec<_>>();
    if let Some((name, _)) = current.split_once(':') { supplied.push(name.trim().to_owned()); }
    else if text.get(offset..).is_some_and(|after| after.starts_with(')'))
        && !current.trim().is_empty()
        && current.trim().chars().all(|c| c.is_ascii_alphanumeric() || c == '_') {
        supplied.push(current.trim().to_owned());
        prefix = "";
    }
    Some((
        &before[callee_start..callee_end],
        prefix,
        supplied,
        chained_invocation,
    ))
}

fn canonical_or_absolute(path: PathBuf) -> PathBuf {
    crate::source::source_path(&path).unwrap_or_else(|_| {
        if path.is_absolute() {
            path
        } else {
            std::env::current_dir().unwrap_or_default().join(path)
        }
    })
}

#[cfg(test)]
mod tests {
    use std::fs;

    use tempfile::tempdir;

    use super::*;

    #[test]
    fn utf16_positions_round_trip() {
        let index = LineIndex::new("a😀b\nç");
        let offset = "a😀".len();
        assert_eq!(index.position(offset), (0, 3));
        assert_eq!(index.offset(0, 3), Some(offset));
    }

    #[test]
    fn overlay_and_artifact_member_completion_use_compiler_types() {
        let directory = tempdir().unwrap();
        let path = directory.path().join("main.req");
        let disk = "artifact Sample { mass: Mass }\nprocess P { gets sample: Sample () => { requires true } }\n";
        fs::write(&path, disk).unwrap();
        let overlay = disk.replace("requires true", "requires sample.mass > 0 Mass");
        let offset = overlay.find("sample.mass").unwrap() + "sample.m".len();
        let mut host = AnalysisHost::new(CompilerOptions::default());
        host.set_file(&path, overlay);
        let snapshot = host.snapshot(&path).unwrap();
        let completions = snapshot.completion_at(&path, offset);
        assert_eq!(completions.len(), 1);
        assert_eq!(completions[0].label, "mass");
        assert!(
            !completions
                .iter()
                .any(|candidate| candidate.label == "append")
        );
    }

    #[test]
    fn completes_process_configuration_invocation_and_run_ports() {
        let directory = tempdir().unwrap();
        let path = directory.path().join("main.req");
        let source = r#"
            artifact A {}
            process Child {
                expose config size: Length = 1 Meter
                gets input: A
                export output: A
                (count: Integer) => {}
            }
            process Parent {
                gets sample: A
                config size: Length = 2 Meter
                config count: Integer = 1
                () => {
                    let configured = Child(size: 1 Meter)
                    let run = configured(count: 1)
                    let direct = Child(size: 2 Meter)(count: 2)
                    requires run.output == run.output
                }
            }
        "#;
        fs::write(&path, source).unwrap();
        let mut host = AnalysisHost::new(CompilerOptions::default());
        let snapshot = host.snapshot(&path).unwrap();

        let config_offset = source.find("size: 1").unwrap() + 2;
        let config = snapshot.completion_at(&path, config_offset);
        assert_eq!(
            config
                .iter()
                .map(|item| item.label.as_str())
                .collect::<Vec<_>>(),
            ["size"]
        );
        assert_eq!(config[0].insert_text, "size");
        assert!(config[0].detail.contains("default = 1 Meter"));
        assert!(config[0].detail.contains("exposed configuration"));
        let config_signature = snapshot.signature_at(&path, config_offset).unwrap();
        assert!(config_signature.label.starts_with("Child configuration("));
        assert!(config_signature.label.contains("size: Length = 1 Meter"));

        let invocation_offset = source.find("count: 1").unwrap() + 2;
        let invocation = snapshot.completion_at(&path, invocation_offset);
        assert_eq!(
            invocation
                .iter()
                .map(|item| item.label.as_str())
                .collect::<Vec<_>>(),
            ["count"]
        );
        assert_eq!(invocation[0].insert_text, "count");
        let invocation_signature = snapshot.signature_at(&path, invocation_offset).unwrap();
        assert!(
            invocation_signature
                .label
                .starts_with("configured invocation(")
        );

        let chained_offset = source.find("count: 2").unwrap() + 2;
        let chained = snapshot.completion_at(&path, chained_offset);
        assert_eq!(
            chained
                .iter()
                .map(|item| item.label.as_str())
                .collect::<Vec<_>>(),
            ["count"]
        );

        let port_offset = source.find("run.output").unwrap() + "run.".len();
        let ports = snapshot.completion_at(&path, port_offset);
        assert!(ports.iter().any(|item| item.label == "output"));
        assert!(ports.iter().any(|item| item.label == "input"));
        assert!(!ports.iter().any(|item| item.label == "size"));
    }

    #[test]
    fn completes_provisioned_resource_exports_and_invocation_claims() {
        let directory = tempdir().unwrap();
        let path = directory.path().join("main.req");
        let source = r#"
            resource Slot { capacity: 4 }
            process SetupPool {
                export slots: Slot[2]
                () => { slots = Slot[2] }
            }
            process Work {
                uses slot: Slot[claim]
                (claim: Integer) => {}
            }
            process Factory {
                () => {
                    let pool = SetupPool()()
                    let work = Work()(claim: 1)
                    pool.slots -> work.slot
                }
            }
        "#;
        fs::write(&path, source).unwrap();
        let mut host = AnalysisHost::new(CompilerOptions::default());
        let snapshot = host.snapshot(&path).unwrap();

        let export_offset = source.find("pool.slots").unwrap() + "pool.la".len();
        let exports = snapshot.completion_at(&path, export_offset);
        let slots = exports
            .iter()
            .find(|candidate| candidate.label == "slots")
            .expect("resource export completion");
        assert!(slots.detail.contains("Slot[2]"), "{slots:?}");

        let uses_offset = source.find("work.slot").unwrap() + "work.la".len();
        let uses = snapshot.completion_at(&path, uses_offset);
        assert!(uses.iter().any(|candidate| {
            candidate.label == "slot" && candidate.detail.contains("Slot[claim]")
        }));

        let claim_offset = source.find("claim: 1").unwrap() + "cl".len();
        let claims = snapshot.completion_at(&path, claim_offset);
        assert!(claims.iter().any(|candidate| {
            candidate.label == "claim" && matches!(candidate.kind, CompletionKind::Parameter)
        }));
    }

    #[test]
    fn unsaved_import_overlay_affects_dependent_completion() {
        let directory = tempdir().unwrap();
        let model_path = directory.path().join("model.req");
        let entry_path = directory.path().join("main.req");
        fs::write(&model_path, "artifact Sample { mass: Mass }\n").unwrap();
        let entry = r#"import "model.req" (Sample)
process P { gets sample: Sample () => { requires sample.volume > 0 Volume } }
"#;
        fs::write(&entry_path, entry).unwrap();

        let mut host = AnalysisHost::new(CompilerOptions::default());
        host.set_file(
            &model_path,
            "artifact Sample { mass: Mass volume: Volume }\n",
        );
        let snapshot = host.snapshot(&entry_path).unwrap();
        let offset = entry.find("sample.volume").unwrap() + "sample.v".len();
        let completions = snapshot.completion_at(&entry_path, offset);
        assert_eq!(
            completions
                .iter()
                .map(|candidate| candidate.label.as_str())
                .collect::<Vec<_>>(),
            ["volume"]
        );
    }

    #[test]
    fn does_not_complete_a_binding_before_its_declaration() {
        let directory = tempdir().unwrap();
        let path = directory.path().join("main.req");
        let source = r#"
            artifact A { mass: Mass }
            process P {
                () => {
                    requires later.mass > 0 Mass
                    let later = A(mass: 1 Kilogram)
                }
            }
        "#;
        fs::write(&path, source).unwrap();
        let mut host = AnalysisHost::new(CompilerOptions::default());
        let snapshot = host.snapshot(&path).unwrap();
        let offset = source.find("later.mass").unwrap() + "later.m".len();
        assert!(snapshot.completion_at(&path, offset).is_empty());
    }

    #[test]
    fn completes_native_math_range() {
        let directory = tempdir().unwrap();
        let path = directory.path().join("main.req");
        let source = r#"
            import math
            process P {
                () => {
                    for value in math.ra {
                        requires value >= 0
                    }
                }
            }
        "#;
        fs::write(&path, source).unwrap();
        let mut host = AnalysisHost::new(CompilerOptions::default());
        let snapshot = host.snapshot(&path).unwrap();
        let offset = source.find("math.ra").unwrap() + "math.ra".len();
        let completions = snapshot.completion_at(&path, offset);
        assert_eq!(
            completions
                .iter()
                .map(|candidate| candidate.label.as_str())
                .collect::<Vec<_>>(),
            ["range"]
        );
    }

    #[test]
    fn completes_visible_bindings_inside_physical_repeat_blocks() {
        let directory = tempdir().unwrap();
        let path = directory.path().join("main.req");
        let source = r#"
            process Stage {}
            process Factory {
                () => {
                    let configured_stage = Stage()
                    let preparing = Stage()()
                    let processing = Stage()()
                    repeat 2 {
                        configured_
                    }
                }
            }
        "#;
        fs::write(&path, source).unwrap();
        let mut host = AnalysisHost::new(CompilerOptions::default());
        let snapshot = host.snapshot(&path).unwrap();
        let offset = source.find("configured_\n").unwrap() + "configured_".len();
        let completions = snapshot.completion_at(&path, offset);
        assert_eq!(
            completions
                .iter()
                .map(|candidate| candidate.label.as_str())
                .collect::<Vec<_>>(),
            ["configured_stage"]
        );
    }

    #[test]
    fn completes_standard_stream_zip() {
        let directory = tempdir().unwrap();
        let path = directory.path().join("main.req");
        let source = r#"
            import std.stream
            process P {
                () => {
                    std.stream.zi
                }
            }
        "#;
        fs::write(&path, source).unwrap();
        let mut host = AnalysisHost::new(CompilerOptions::default());
        let snapshot = host.snapshot(&path).unwrap();
        let offset = source.find("std.stream.zi").unwrap() + "std.stream.zi".len();
        let completions = snapshot.completion_at(&path, offset);
        assert_eq!(
            completions
                .iter()
                .map(|candidate| candidate.label.as_str())
                .collect::<Vec<_>>(),
            ["zip", "zip_exact"]
        );
    }

    #[test]
    fn completes_monitor_metrics_as_keyword_and_display_units() {
        let directory = tempdir().unwrap();
        let path = directory.path().join("main.req");
        let source = r#"
            unit MillionUSD = USD * 1000000
            process CostAccounting {
                export cost: USD aggregate sum
            }
            process Factory: CostAccounting {
                monitor cost as MillionUSD
                () => { cost = 2500000 USD }
            }
        "#;
        fs::write(&path, source).unwrap();
        let mut host = AnalysisHost::new(CompilerOptions::default());
        let snapshot = host.snapshot(&path).unwrap();

        let metric_offset = source.find("monitor cost").unwrap() + "monitor co".len();
        let metric = snapshot.completion_at(&path, metric_offset);
        assert!(metric.iter().any(|candidate| candidate.label == "cost"));

        let as_offset = source.find("monitor cost as").unwrap() + "monitor cost a".len();
        let keyword = snapshot.completion_at(&path, as_offset);
        assert_eq!(keyword.len(), 1);
        assert_eq!(keyword[0].label, "as");

        let unit_offset =
            source.find("monitor cost as MillionUSD").unwrap() + "monitor cost as Million".len();
        let units = snapshot.completion_at(&path, unit_offset);
        assert!(
            units
                .iter()
                .any(|candidate| candidate.label == "MillionUSD")
        );
    }
}
