//! Compiler-owned source edits and completion additions. Offsets are UTF-8 bytes;
//! protocol adapters convert them to their editor's coordinate system.
use std::path::Path;

use crate::ast::{ImportPath, Item, MemberRole};
use crate::ide::{AnalysisSnapshot, CompletionCandidate, CompletionKind};
use crate::source::SourceFile;

#[derive(Clone, Debug)]
pub struct SourceEdit {
    pub start: usize,
    pub end: usize,
    pub new_text: String,
}

#[derive(Clone, Debug)]
pub struct EditingAction {
    pub title: String,
    pub edits: Vec<SourceEdit>,
    /// Snippets require an editor command; ordinary edits never contain snippets.
    pub snippet: Option<SourceEdit>,
}

impl AnalysisSnapshot {
    pub(crate) fn editing_source(&self, path: &Path) -> Option<&SourceFile> {
        let canonical = crate::source::source_path(path).ok()?;
        self.sources.files.get(canonical.to_str()?)
    }

    pub fn formatting_edits(&self, path: &Path) -> Result<Vec<SourceEdit>, String> {
        let source = self.editing_source(path).ok_or("document unavailable")?;
        let formatted = crate::format_source(&source.text).map_err(|e| e.to_string())?;
        Ok(if formatted == source.text {
            vec![]
        } else {
            vec![SourceEdit {
                start: 0,
                end: source.text.len(),
                new_text: formatted,
            }]
        })
    }

    /// Scope-preserving fixes only. Unknown values remain explicit user-editable
    /// placeholders; requirements and numeric literals are never weakened.
    pub fn code_actions_at(&self, path: &Path, start: usize, end: usize) -> Vec<EditingAction> {
        let Some(source) = self.editing_source(path) else {
            return vec![];
        };
        let mut actions = Vec::new();
        for item in &source.parse.module.items {
            if let Item::Process(process) = item {
                for member in &process.members {
                    if member.role == MemberRole::Config
                        && !member.exposed
                        && member.span.start <= end
                        && start <= member.span.end
                        && source.text[member.span.start..].starts_with("config")
                    {
                        actions.push(EditingAction {
                            title: format!("Expose config {}", member.name),
                            edits: vec![SourceEdit {
                                start: member.span.start,
                                end: member.span.start,
                                new_text: "expose ".into(),
                            }],
                            snippet: None,
                        });
                    }
                }
            }
        }
        // Offer qualification/import fixes only for a compiler-reported unknown
        // identifier and an exact declaration in the loaded module closure.
        for diagnostic in &self.compilation.report.diagnostics {
            if diagnostic.span.path != source.display_path
                || diagnostic.span.start > end
                || start > diagnostic.span.end
            {
                continue;
            }
            let Some(name) = diagnostic
                .message
                .strip_prefix("unknown name ")
                .or_else(|| {
                    diagnostic
                        .message
                        .strip_prefix("unknown function, constructor, or process call ")
                })
            else {
                continue;
            };
            if !name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') {
                continue;
            }
            let identifier_end = diagnostic.span.start + name.len();
            if source.text.get(diagnostic.span.start..identifier_end) != Some(name)
                || source.text.get(identifier_end..).is_some_and(|rest| {
                    rest.starts_with(|c: char| c.is_ascii_alphanumeric() || c == '_')
                })
            {
                continue;
            }
            let mut imported_targets = std::collections::HashSet::new();
            for item in &source.parse.module.items {
                let Item::Import(import) = item else { continue };
                let Some(key) =
                    crate::namespaces::target_key(&self.sources, &source.key, &import.path)
                else {
                    continue;
                };
                imported_targets.insert(key.clone());
                let Some(target) = self.sources.files.get(&key) else {
                    continue;
                };
                if !target
                    .parse
                    .module
                    .items
                    .iter()
                    .any(|i| i.name() == Some(name))
                {
                    continue;
                }
                if let Some(alias) = &import.alias {
                    actions.push(EditingAction {
                        title: format!("Qualify as {alias}.{name}"),
                        edits: vec![SourceEdit {
                            start: diagnostic.span.start,
                            end: identifier_end,
                            new_text: format!("{alias}.{name}"),
                        }],
                        snippet: None,
                    });
                } else if !import.names.iter().any(|n| n == name) {
                    let mut names = import.names.clone();
                    names.push(name.into());
                    let module = match &import.path {
                        ImportPath::Relative(p) => crate::ast::quote_string(p),
                        ImportPath::Module(p) => p.clone(),
                    };
                    actions.push(EditingAction {
                        title: format!("Import {name} from {module}"),
                        edits: vec![SourceEdit {
                            start: import.span.start,
                            end: import.span.end,
                            new_text: format!("import {module} ({})", names.join(", ")),
                        }],
                        snippet: None,
                    });
                }
            }
            for (key, target) in &self.sources.files {
                if key == &source.key
                    || imported_targets.contains(key)
                    || !target
                        .parse
                        .module
                        .items
                        .iter()
                        .any(|i| i.name() == Some(name))
                    || self.import_reaches(key, &source.key, &mut std::collections::HashSet::new())
                {
                    continue;
                }
                let module = if let Some(module) = key.strip_prefix("std:") {
                    module.to_owned()
                } else {
                    let Some(parent) = source.path.as_deref().and_then(Path::parent) else {
                        continue;
                    };
                    let Some(path) = target.path.as_deref() else {
                        continue;
                    };
                    let Some(relative) = relative_import_path(parent, path) else {
                        continue;
                    };
                    crate::ast::quote_string(&relative)
                };
                actions.push(EditingAction {
                    title: format!("Import {name} from {module}"),
                    edits: vec![SourceEdit {
                        start: 0,
                        end: 0,
                        new_text: format!("import {module} ({name})\n"),
                    }],
                    snippet: None,
                });
            }
        }
        // An argument-list insertion is offered at its closing parenthesis, using
        // the same signature and supplied-argument logic as completion.
        if let Some((close, candidates)) = self.missing_argument_candidates(path, start) {
            if !candidates.is_empty() {
                let before = &source.text[..close];
                let prefix = if before.trim_end().ends_with(['(', ',']) {
                    ""
                } else {
                    ", "
                };
                let fields = candidates
                    .iter()
                    .enumerate()
                    .map(|(index, candidate)| {
                        format!(
                            "{}: ${{{}:TODO_{}}}",
                            candidate.label,
                            index + 1,
                            candidate.label
                        )
                    })
                    .collect::<Vec<_>>()
                    .join(", ");
                actions.push(EditingAction {
                    title: "Insert missing named arguments…".into(),
                    edits: vec![],
                    snippet: Some(SourceEdit {
                        start: close,
                        end: close,
                        new_text: format!("{prefix}{fields}"),
                    }),
                });
            }
        }
        actions
    }

    fn import_reaches(
        &self,
        from: &str,
        target: &str,
        seen: &mut std::collections::HashSet<String>,
    ) -> bool {
        if from == target {
            return true;
        }
        if !seen.insert(from.into()) {
            return false;
        }
        self.sources.files.get(from).is_some_and(|file| {
            file.parse.module.items.iter().any(|item| {
                let Item::Import(import) = item else {
                    return false;
                };
                crate::namespaces::target_key(&self.sources, from, &import.path)
                    .is_some_and(|next| self.import_reaches(&next, target, seen))
            })
        })
    }

    fn missing_argument_candidates(
        &self,
        path: &Path,
        offset: usize,
    ) -> Option<(usize, Vec<CompletionCandidate>)> {
        let source = self.editing_source(path)?;
        // Restrict to the current line and a simple argument list; nested values
        // are intentionally left to completion until a complete AST edit exists.
        let line_end = source
            .text
            .get(offset..)?
            .find('\n')
            .map_or(source.text.len(), |n| offset + n);
        let close = offset + source.text.get(offset..line_end)?.find(')')?;
        if !named_argument_list(&source.text[..close]) {
            return None;
        }
        let signature = self.signature_at(path, close)?;
        let candidates = self
            .completion_at(path, close)
            .into_iter()
            .filter(|c| {
                signature
                    .parameters
                    .iter()
                    .any(|p| p.starts_with(&format!("{}:", c.label)) && !p.contains(" = "))
            })
            .collect();
        Some((close, candidates))
    }

    pub(crate) fn extended_completion_at(
        &self,
        path: &Path,
        offset: usize,
    ) -> Option<Vec<CompletionCandidate>> {
        let source = self.editing_source(path)?;
        let before = source.text.get(..offset)?;
        let word_start = before
            .rfind(|c: char| !c.is_ascii_alphanumeric() && c != '_' && c != '.')
            .map_or(0, |n| n + 1);
        let word = &before[word_start..];
        if let Some((receiver, prefix)) = word.rsplit_once('.') {
            let function = source
                .parse
                .module
                .items
                .iter()
                .find_map(|item| match item {
                    Item::Function(function) if function.span.contains(&offset) => {
                        self.compilation.model.functions.values().find(|model| {
                            model.path == format!("{}::{}", source.display_path, function.name)
                        })
                    }
                    _ => None,
                });
            if let Some(function) = function {
                let mut parts = receiver.split('.');
                let mut ty = parts.next().and_then(|name| function.parameters.get(name));
                for field in parts {
                    ty = ty.and_then(|ty| match ty {
                        crate::types::Type::NamedTuple(fields) => fields
                            .iter()
                            .find(|(name, _)| name == field)
                            .map(|(_, ty)| ty),
                        _ => None,
                    });
                }
                if let Some(crate::types::Type::NamedTuple(fields)) = ty {
                    return Some(
                        fields
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
                    );
                }
            }
        }
        if let Some((alias, prefix)) = word.split_once('.') {
            for item in &source.parse.module.items {
                let Item::Import(import) = item else { continue };
                if import.alias.as_deref() != Some(alias) {
                    continue;
                }
                let target =
                    crate::namespaces::target_key(&self.sources, &source.key, &import.path)?;
                let target = self.sources.files.get(&target)?;
                return Some(
                    target
                        .parse
                        .module
                        .items
                        .iter()
                        .filter_map(|item| {
                            let name = item.name()?;
                            name.starts_with(prefix)
                                .then(|| declaration_candidate(item, name, &target.display_path))
                        })
                        .collect(),
                );
            }
        }
        // A type annotation can occur at module scope (artifact/tuple fields),
        // so it must not depend on locating an enclosing process.
        let line_start = before.rfind('\n').map_or(0, |n| n + 1);
        let lead = before[line_start..word_start].trim_end();
        if !lead.ends_with(':') || lead.contains('=') {
            return None;
        }
        let mut candidates = vec![];
        for item in &source.parse.module.items {
            match item {
                Item::Import(import) if import.alias.is_none() => {
                    if let Some(target) =
                        crate::namespaces::target_key(&self.sources, &source.key, &import.path)
                            .and_then(|k| self.sources.files.get(&k))
                    {
                        for item in &target.parse.module.items {
                            if let Some(name) = item.name().filter(|n| {
                                import.names.iter().any(|i| i == n) && n.starts_with(word)
                            }) {
                                if matches!(item, Item::Unit(_) | Item::Artifact(_)) {
                                    candidates.push(declaration_candidate(
                                        item,
                                        name,
                                        &target.display_path,
                                    ));
                                }
                            }
                        }
                    }
                }
                Item::Unit(_) | Item::Artifact(_) => {
                    if let Some(name) = item.name().filter(|n| n.starts_with(word)) {
                        candidates.push(declaration_candidate(item, name, &source.display_path));
                    }
                }
                _ => {}
            }
        }
        for name in [
            "Integer", "Scalar", "Bool", "Length", "Time", "Mass", "Area", "Volume", "Meter",
            "Second", "USD",
        ] {
            if name.starts_with(word) {
                candidates.push(CompletionCandidate {
                    label: name.into(),
                    kind: CompletionKind::Unit,
                    detail: "built-in type".into(),
                    insert_text: name.into(),
                    definition: None,
                });
            }
        }
        candidates.sort_by(|a, b| a.label.cmp(&b.label));
        candidates.dedup_by(|a, b| a.label == b.label);
        Some(candidates)
    }

    pub(crate) fn resolve_visible_name(&self, source: &SourceFile, name: &str) -> Option<String> {
        let (target, local) = if let Some((alias, member)) = name.split_once('.') {
            let import = source
                .parse
                .module
                .items
                .iter()
                .find_map(|item| match item {
                    Item::Import(import) if import.alias.as_deref() == Some(alias) => Some(import),
                    _ => None,
                })?;
            let target = crate::namespaces::target_key(&self.sources, &source.key, &import.path)?;
            (self.sources.files.get(&target)?, member)
        } else if source
            .parse
            .module
            .items
            .iter()
            .any(|item| item.name() == Some(name))
        {
            (source, name)
        } else {
            let import = source
                .parse
                .module
                .items
                .iter()
                .find_map(|item| match item {
                    Item::Import(import)
                        if import.alias.is_none() && import.names.iter().any(|n| n == name) =>
                    {
                        Some(import)
                    }
                    _ => None,
                })?;
            let target = crate::namespaces::target_key(&self.sources, &source.key, &import.path)?;
            (self.sources.files.get(&target)?, name)
        };
        if !target
            .parse
            .module
            .items
            .iter()
            .any(|item| item.name() == Some(local))
        {
            return None;
        }
        let qualified = format!("{}::{local}", target.display_path);
        self.compilation
            .model
            .processes
            .iter()
            .find(|(_, value)| value.path == qualified)
            .map(|(key, _)| key.clone())
            .or_else(|| {
                self.compilation
                    .model
                    .artifacts
                    .iter()
                    .find(|(_, value)| value.path == qualified)
                    .map(|(key, _)| key.clone())
            })
            .or_else(|| {
                self.compilation
                    .model
                    .functions
                    .iter()
                    .find(|(_, value)| value.path == qualified)
                    .map(|(key, _)| key.clone())
            })
            .or_else(|| Some(local.into()))
    }
}

fn relative_import_path(parent: &Path, target: &Path) -> Option<String> {
    let parent = parent.components().collect::<Vec<_>>();
    let target = target.components().collect::<Vec<_>>();
    let common = parent
        .iter()
        .zip(&target)
        .take_while(|(a, b)| a == b)
        .count();
    if common == 0 {
        return None;
    }
    let mut path = std::path::PathBuf::new();
    for _ in common..parent.len() {
        path.push("..");
    }
    for component in &target[common..] {
        path.push(component.as_os_str());
    }
    Some(path.to_string_lossy().replace('\\', "/"))
}

fn named_argument_list(before: &str) -> bool {
    use crate::syntax::SyntaxKind::*;
    let tokens = crate::lexer::lex(before)
        .tokens
        .into_iter()
        .filter(|t| !t.is_trivia())
        .collect::<Vec<_>>();
    let mut stack = Vec::new();
    for (index, token) in tokens.iter().enumerate() {
        match token.kind {
            LParen => stack.push(index),
            RParen => {
                stack.pop();
            }
            _ => {}
        }
    }
    let Some(open) = stack.pop() else {
        return false;
    };
    let mut depth = 0;
    let mut segments = Vec::new();
    let mut begin = open + 1;
    for (index, token) in tokens.iter().enumerate().skip(begin) {
        match token.kind {
            LParen | LBracket | LBrace => depth += 1,
            RParen | RBracket | RBrace => depth -= 1,
            Comma if depth == 0 => {
                segments.push(&tokens[begin..index]);
                begin = index + 1;
            }
            _ => {}
        }
    }
    segments.push(&tokens[begin..]);
    segments.into_iter().all(|segment| {
        segment.is_empty()
            || (segment[0].kind == Ident && (segment.len() == 1 || segment[1].kind == Colon))
    })
}

fn declaration_candidate(item: &Item, name: &str, path: &str) -> CompletionCandidate {
    let (kind, detail) = match item {
        Item::Unit(unit) if unit.refinement.is_some() => (CompletionKind::Unit, "refinement type"),
        Item::Unit(_) => (CompletionKind::Unit, "unit"),
        Item::Function(_) => (CompletionKind::Method, "pure function"),
        Item::Process(_) => (CompletionKind::Configuration, "process"),
        Item::Artifact(_) => (CompletionKind::Field, "artifact type"),
        _ => (CompletionKind::Field, "declaration"),
    };
    CompletionCandidate {
        label: name.into(),
        kind,
        detail: detail.into(),
        insert_text: name.into(),
        definition: Some(path.into()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{AnalysisHost, CompilerOptions};

    fn snapshot(text: &str) -> (tempfile::TempDir, std::path::PathBuf, AnalysisSnapshot) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("main.req");
        std::fs::write(&path, "process Disk {}\n").unwrap();
        let mut host = AnalysisHost::new(CompilerOptions::default());
        host.set_file(&path, text);
        let result = host.snapshot(&path).unwrap();
        (dir, path, result)
    }

    #[test]
    fn formatting_uses_unsaved_source_and_is_idempotent() {
        let text = "process P {config x:Scalar=1  \r\n}\r\n\r\n";
        let (_dir, path, snap) = snapshot(text);
        let edits = snap.formatting_edits(&path).unwrap();
        assert_eq!(edits.len(), 1);
        assert_eq!(edits[0].end, text.len());
        let (_dir, path, snap) = snapshot(&edits[0].new_text);
        assert!(snap.formatting_edits(&path).unwrap().is_empty());
        let (_dir, path, snap) = snapshot("process {");
        assert!(snap.formatting_edits(&path).is_err());
    }

    #[test]
    fn completes_refinement_types_and_nested_tuple_fields() {
        let text = "unit Positive = Scalar(x) { requires x > 0 }\nprocess P {\nconfig size: Positive = 1\nconfig settings: (module: (width: Integer, height: Integer)) = (module: (width: 2, height: 3))\n() => { let width = settings.module.width }\n}\n";
        let (_dir, path, snap) = snapshot(text);
        let offset = text.find("size: Positive").unwrap() + "size: Pos".len();
        assert!(
            snap.completion_at(&path, offset)
                .iter()
                .any(|c| c.label == "Positive" && c.detail.contains("refinement"))
        );
        let offset = text.find("settings.module.width").unwrap() + "settings.module.w".len();
        assert!(
            snap.completion_at(&path, offset)
                .iter()
                .any(|c| c.label == "width")
        );
    }

    #[test]
    fn completes_named_tuple_function_parameters() {
        let text = "fn width(settings: (module: (width: Integer, height: Integer))) -> (result: Integer) { result = settings.module.width }\n";
        let (_dir, path, snap) = snapshot(text);
        let offset = text.find("settings.module.width").unwrap() + "settings.module.w".len();
        let candidates = snap.completion_at(&path, offset);
        assert!(
            candidates.iter().any(|c| c.label == "width"),
            "{candidates:?}"
        );
    }

    #[test]
    fn excludes_supplied_arguments_with_nested_values_and_punning() {
        let text = "process Child { config settings: (x: Integer, y: Integer)\nconfig count: Integer\nconfig remaining: Scalar\n}\nprocess P { () => { let count = 1\nlet child = Child(settings: (x: 1, y: 2), count, ) } }\n";
        let (_dir, path, snap) = snapshot(text);
        let offset = text.find("count, )").unwrap() + "count, ".len();
        let names = snap
            .completion_at(&path, offset)
            .into_iter()
            .map(|c| c.label)
            .collect::<Vec<_>>();
        assert_eq!(names, vec!["remaining"]);
        let actions = snap.code_actions_at(&path, offset, offset);
        assert!(actions.iter().any(|a| {
            a.snippet
                .as_ref()
                .is_some_and(|s| s.new_text.contains("remaining: ${1:TODO_remaining}"))
        }));
    }

    #[test]
    fn namespace_completions_and_qualification_use_exact_exports() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("main.req");
        std::fs::write(
            dir.path().join("module.req"),
            "process Worker { config amount: Integer }\n",
        )
        .unwrap();
        let text = "import \"module.req\" as module\nprocess P { () => { let c = module.Worker(amount: 1)\nlet missing = Worker\n} }\n";
        std::fs::write(&path, text).unwrap();
        let mut host = AnalysisHost::new(CompilerOptions::default());
        let snap = host.snapshot(&path).unwrap();
        let offset = text.find("module.Worker").unwrap() + "module.W".len();
        assert_eq!(snap.completion_at(&path, offset)[0].label, "Worker");
        let offset = text.find("amount: 1").unwrap();
        assert!(
            snap.completion_at(&path, offset)
                .iter()
                .any(|c| c.label == "amount")
        );
        let offset = text.find("missing = Worker").unwrap() + "missing = ".len();
        let actions = snap.code_actions_at(&path, offset, offset + "Worker".len());
        assert!(
            actions
                .iter()
                .any(|a| a.title == "Qualify as module.Worker"),
            "{actions:?}"
        );
    }

    #[test]
    fn expose_config_action_has_no_value_changes() {
        let text = "process P { config size: Integer = 1\n}\n";
        let (_dir, path, snap) = snapshot(text);
        let offset = text.find("size").unwrap();
        let action = snap
            .code_actions_at(&path, offset, offset)
            .into_iter()
            .find(|a| a.title == "Expose config size")
            .unwrap();
        assert_eq!(action.edits[0].new_text, "expose ");
        assert_eq!(action.edits[0].start, action.edits[0].end);
    }

    #[test]
    fn adds_exact_missing_selective_import_without_guessing_values() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("main.req");
        std::fs::write(
            dir.path().join("module.req"),
            "process Worker {}\nprocess Task {}\n",
        )
        .unwrap();
        let text = "import \"module.req\" (Task)\nprocess P { () => { let c = Worker() } }\n";
        std::fs::write(&path, text).unwrap();
        let mut host = AnalysisHost::new(CompilerOptions::default());
        let snap = host.snapshot(&path).unwrap();
        let offset = text.find("Worker()").unwrap();
        let actions = snap.code_actions_at(&path, offset, offset + 6);
        let edit = &actions
            .iter()
            .find(|a| a.title.starts_with("Import Worker"))
            .unwrap()
            .edits[0];
        let mut fixed = text.to_owned();
        fixed.replace_range(edit.start..edit.end, &edit.new_text);
        host.set_file(&path, fixed);
        let fixed = host.snapshot(&path).unwrap();
        assert!(
            fixed.compilation.report.diagnostics.is_empty(),
            "{:?}",
            fixed.compilation.report.diagnostics
        );
    }

    #[test]
    fn imports_transitive_declaration_without_creating_cycle() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("main.req");
        std::fs::write(dir.path().join("module.req"), "process Worker {}\n").unwrap();
        std::fs::write(
            dir.path().join("bridge.req"),
            "import \"module.req\" (Worker)\nprocess Bridge {}\n",
        )
        .unwrap();
        let text = "import \"bridge.req\" (Bridge)\nprocess P { () => { let c = Worker() } }\n";
        std::fs::write(&path, text).unwrap();
        let mut host = AnalysisHost::new(CompilerOptions::default());
        let snap = host.snapshot(&path).unwrap();
        let offset = text.find("Worker()").unwrap();
        let actions = snap.code_actions_at(&path, offset, offset);
        let edit = &actions
            .iter()
            .find(|a| a.title == "Import Worker from \"module.req\"")
            .unwrap()
            .edits[0];
        assert_eq!(edit.new_text, "import \"module.req\" (Worker)\n");
        assert_eq!(edit.start, 0);
        assert!(
            snap.import_reaches(
                path.canonicalize().unwrap().to_str().unwrap(),
                dir.path()
                    .join("module.req")
                    .canonicalize()
                    .unwrap()
                    .to_str()
                    .unwrap(),
                &mut std::collections::HashSet::new()
            )
        );
    }
}
